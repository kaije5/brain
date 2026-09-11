use std::time::Duration;

use chrono::{DateTime, Utc};
use cortex_application::{ApplicationError, RecoveryHint};

/// Bounded length for sanitized diagnostic messages carried on provider
/// errors: long enough for actionable context, short enough for IPC and logs.
const MAX_ERROR_MESSAGE_BYTES: usize = 256;

/// Classified provider/inference failure categories at the transport
/// boundary. Each category carries an explicit recovery policy so consumers
/// never branch on error strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFailureCategory {
    /// Connect/request/stale-stream timeout.
    Timeout,
    /// Transient connect, DNS, or service-unavailable failure.
    Unavailable,
    /// True provider throttling (usually HTTP 429 with a rate-limit code).
    RateLimit,
    /// Structured provider overload signal.
    Overloaded,
    /// Remaining transient 5xx provider errors.
    ServerError,
    /// Invalid, expired, or unauthorized credential.
    Auth,
    /// Insufficient quota, credits, or billing-required structured error.
    QuotaOrBilling,
    /// The request exceeded the selected model's context window.
    ContextOverflow,
    /// Unsupported model/parameter or other caller/config-side 4xx.
    InvalidRequest,
    /// Invalid provider response or protocol payload.
    MalformedResponse,
}

impl ProviderFailureCategory {
    /// Deterministic recovery policy for this category. Retries target the
    /// same resolved selection only; there is no fallback (ADR-024).
    #[must_use]
    pub const fn recovery_hint(self) -> RecoveryHint {
        match self {
            Self::Timeout
            | Self::Unavailable
            | Self::RateLimit
            | Self::Overloaded
            | Self::ServerError => RecoveryHint::RetrySameSelection,
            Self::ContextOverflow => RecoveryHint::CompactContext,
            Self::Auth | Self::QuotaOrBilling | Self::InvalidRequest | Self::MalformedResponse => {
                RecoveryHint::Abort
            }
        }
    }

    /// Whether retrying the same resolved selection may succeed.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self.recovery_hint(), RecoveryHint::RetrySameSelection)
    }
}

/// A classified, sanitized provider failure. Structured metadata (HTTP
/// status, provider error code, parsed `Retry-After`) travels with the
/// category; the message is bounded, control-character free, and never
/// contains credentials, authorization headers, or raw response bodies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError {
    category: ProviderFailureCategory,
    status: Option<u16>,
    provider_code: Option<String>,
    retry_after: Option<Duration>,
    message: String,
}

impl ProviderError {
    /// A minimal classified error without structured metadata.
    #[must_use]
    pub fn from_category(category: ProviderFailureCategory) -> Self {
        Self {
            category,
            status: None,
            provider_code: None,
            retry_after: None,
            message: String::new(),
        }
    }

    #[must_use]
    pub const fn category(&self) -> ProviderFailureCategory {
        self.category
    }

    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    #[must_use]
    pub fn provider_code(&self) -> Option<&str> {
        self.provider_code.as_deref()
    }

    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "provider error ({:?})", self.category)?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if let Some(code) = self.provider_code.as_deref() {
            write!(formatter, " code={code}")?;
        }
        if !self.message.is_empty() {
            write!(formatter, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

/// Parses a `Retry-After` header value in delta-seconds or HTTP-date form.
/// Invalid values yield `None` rather than panicking; past dates yield zero.
#[must_use]
pub fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let value = value?.to_str().ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<i64>() {
        return Some(Duration::from_secs(seconds.max(0).cast_unsigned()));
    }
    let date = DateTime::parse_from_rfc2822(trimmed).ok()?;
    let remaining = date.with_timezone(&Utc) - Utc::now();
    if remaining <= chrono::TimeDelta::zero() {
        Some(Duration::ZERO)
    } else {
        remaining.to_std().ok()
    }
}

/// Classifies a transport-level failure (send, connect, or body read).
/// Permanent TLS/configuration failures abort; transient network failures
/// remain retryable.
#[must_use]
pub fn classify_network_error(error: &reqwest::Error) -> ProviderError {
    let category = if error.is_timeout() {
        ProviderFailureCategory::Timeout
    } else if error.is_connect() && is_permanent_connect_failure(error) {
        ProviderFailureCategory::InvalidRequest
    } else if error.is_decode() || error.is_body() {
        ProviderFailureCategory::MalformedResponse
    } else {
        ProviderFailureCategory::Unavailable
    };
    ProviderError {
        category,
        status: error.status().map(|status| status.as_u16()),
        provider_code: None,
        retry_after: None,
        message: String::new(),
    }
}

/// TLS/certificate failures are configuration defects: retrying them would
/// never succeed, so they must abort rather than loop as "unavailable".
fn is_permanent_connect_failure(error: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        let text = current.to_string();
        let lowered = text.to_ascii_lowercase();
        if lowered.contains("certificate")
            || lowered.contains("tls")
            || lowered.contains("handshake")
            || lowered.contains("invalid dns name")
        {
            return true;
        }
        source = current.source();
    }
    false
}

/// Structured OpenAI-compatible provider error payload
/// (`{"error": {"message", "type", "code"}}`); `error` may also be a string.
#[derive(serde::Deserialize)]
struct ProviderErrorPayload<'a> {
    #[serde(borrow, default)]
    error: Option<ProviderErrorDetail<'a>>,
}

#[derive(serde::Deserialize)]
struct ProviderErrorDetail<'a> {
    #[serde(borrow, default)]
    message: Option<&'a str>,
    #[serde(borrow, default)]
    r#type: Option<&'a str>,
    #[serde(borrow, default)]
    code: Option<ProviderCode<'a>>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ProviderCode<'a> {
    Text(&'a str),
    Number(i64),
}

/// Quota/billing structured codes: a 429 carrying one of these is billing
/// exhaustion, not throttling, and must not be retried.
const QUOTA_CODES: &[&str] = &[
    "insufficient_quota",
    "insufficient_credits",
    "quota_exceeded",
    "billing_required",
    "credit_balance_too_low",
    "payment_required",
];

/// Structured context-overflow signals. Structured fields are preferred;
/// these isolated message fragments only apply when structured fields are
/// absent and stay inside this classifier.
const CONTEXT_OVERFLOW_CODES: &[&str] = &[
    "context_length_exceeded",
    "maximum_context_length",
    "context_overflow",
    "request_too_large_for_model",
];
const CONTEXT_OVERFLOW_FRAGMENTS: &[&str] = &[
    "context length",
    "context_length",
    "maximum context",
    "context window",
];

/// Structured overload signals (e.g. Anthropic-style `overloaded_error`,
/// OpenAI-style "model is overloaded").
const OVERLOAD_FRAGMENTS: &[&str] = &["overloaded"];

/// Classifies an HTTP failure response from the provider. The raw body is
/// parsed only for structured error fields; a bounded, sanitized fragment of
/// the structured message (never headers or the full body) is retained.
#[must_use]
pub fn classify_http_response(
    status: u16,
    retry_after: Option<&reqwest::header::HeaderValue>,
    body: &[u8],
) -> ProviderError {
    let retry_after = parse_retry_after(retry_after);
    let (message, provider_type, provider_code) = extract_provider_error_fields(body);
    let lowered_code = provider_code.as_deref().map(str::to_ascii_lowercase);
    let lowered_type = provider_type.as_deref().map(str::to_ascii_lowercase);

    // Structured quota/billing wins over the HTTP status: a 429 with an
    // insufficient-quota code is billing exhaustion, not throttling.
    let quota = lowered_code.as_deref().is_some_and(|code| {
        QUOTA_CODES.contains(&code) || QUOTA_CODES.contains(&normalize_code_prefix(code))
    }) || lowered_type
        .as_deref()
        .is_some_and(|kind| QUOTA_CODES.contains(&kind));
    if quota {
        return ProviderError {
            category: ProviderFailureCategory::QuotaOrBilling,
            status: Some(status),
            provider_code,
            retry_after,
            message,
        };
    }

    let overflow = lowered_code
        .as_deref()
        .is_some_and(|code| CONTEXT_OVERFLOW_CODES.contains(&code))
        || lowered_type
            .as_deref()
            .is_some_and(|kind| CONTEXT_OVERFLOW_CODES.contains(&kind))
        || (status == 400
            && matches_on_fragments(
                &[lowered_code.as_deref(), lowered_type.as_deref()],
                &combined_message(&message, lowered_type.as_deref()),
                CONTEXT_OVERFLOW_FRAGMENTS,
            ));
    if overflow {
        return ProviderError {
            category: ProviderFailureCategory::ContextOverflow,
            status: Some(status),
            provider_code,
            retry_after,
            message,
        };
    }

    let category = match status {
        401 | 403 => ProviderFailureCategory::Auth,
        429 => ProviderFailureCategory::RateLimit,
        500..=599 => {
            if matches_on_fragments(
                &[lowered_type.as_deref()],
                &combined_message(&message, lowered_type.as_deref()),
                OVERLOAD_FRAGMENTS,
            ) {
                ProviderFailureCategory::Overloaded
            } else {
                ProviderFailureCategory::ServerError
            }
        }
        400..=499 => ProviderFailureCategory::InvalidRequest,
        _ => ProviderFailureCategory::MalformedResponse,
    };
    ProviderError {
        category,
        status: Some(status),
        provider_code,
        retry_after,
        message,
    }
}

/// Response-body over-budget failures are protocol violations of the
/// configured allocation limits.
#[must_use]
pub fn response_too_large() -> ProviderError {
    ProviderError {
        category: ProviderFailureCategory::MalformedResponse,
        status: None,
        provider_code: None,
        retry_after: None,
        message: "provider response exceeded configured byte limit".to_owned(),
    }
}

/// Maps a classified provider error onto the public application boundary.
/// Consumers recover from the typed category/recovery hint, never from
/// provider error text.
#[must_use]
pub fn map_provider_error(error: &ProviderError) -> ApplicationError {
    match error.category {
        ProviderFailureCategory::Timeout => ApplicationError::InferenceTimeout,
        ProviderFailureCategory::Unavailable
        | ProviderFailureCategory::Overloaded
        | ProviderFailureCategory::ServerError => ApplicationError::InferenceUnavailable,
        ProviderFailureCategory::RateLimit => ApplicationError::RateLimited {
            retry_after_secs: error.retry_after.map(|delay| delay.as_secs()),
        },
        ProviderFailureCategory::Auth => ApplicationError::AuthenticationFailed,
        ProviderFailureCategory::QuotaOrBilling => ApplicationError::QuotaExceeded,
        ProviderFailureCategory::ContextOverflow => ApplicationError::ContextOverflow,
        ProviderFailureCategory::InvalidRequest => ApplicationError::InvalidInferenceRequest,
        ProviderFailureCategory::MalformedResponse => ApplicationError::MalformedModelOutput {
            reason: "provider response was malformed or exceeded configured limits",
        },
    }
}

/// Reads a response body under a hard byte budget.
///
/// # Errors
/// Returns a classified provider error for transport or over-budget reads.
pub async fn read_bounded_body(
    mut response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<Vec<u8>, ProviderError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(response_too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_network_error(&error))?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(response_too_large)?;
        if next_len > max_response_bytes {
            return Err(response_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn extract_provider_error_fields(body: &[u8]) -> (String, Option<String>, Option<String>) {
    let payload: ProviderErrorPayload<'_> = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(_) => return (String::new(), None, None),
    };
    let Some(detail) = payload.error else {
        return (String::new(), None, None);
    };
    let code = match detail.code {
        Some(ProviderCode::Text(text)) => Some(text.to_owned()),
        Some(ProviderCode::Number(number)) => Some(number.to_string()),
        None => None,
    };
    (
        sanitize_message(detail.message.unwrap_or_default()),
        detail.r#type.map(str::to_owned),
        code,
    )
}

/// Bounded, control-character-free message text. Credentials and
/// authorization material are redacted defensively even though headers never
/// enter the classifier.
fn sanitize_message(raw: &str) -> String {
    let mut sanitized = String::with_capacity(raw.len().min(MAX_ERROR_MESSAGE_BYTES * 2));
    for character in raw.chars() {
        if character.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
        if sanitized.len() >= MAX_ERROR_MESSAGE_BYTES {
            break;
        }
    }
    if sanitized.len() > MAX_ERROR_MESSAGE_BYTES {
        let mut cut = MAX_ERROR_MESSAGE_BYTES;
        while !sanitized.is_char_boundary(cut) {
            cut -= 1;
        }
        sanitized.truncate(cut);
    }
    redact_secrets(sanitized.trim().to_owned())
}

fn redact_secrets(mut message: String) -> String {
    for marker in ["Bearer ", "bearer ", "sk-", "nvapi-"] {
        if let Some(start) = message.find(marker) {
            message.truncate(start);
            message.push_str("<redacted>");
        }
    }
    message
}

fn combined_message(message: &str, provider_type: Option<&str>) -> String {
    let mut combined = String::new();
    if let Some(kind) = provider_type {
        combined.push_str(kind);
        combined.push(' ');
    }
    combined.push_str(message);
    combined.to_ascii_lowercase()
}

fn matches_on_fragments(fields: &[Option<&str>], combined: &str, fragments: &[&str]) -> bool {
    if fields.iter().flatten().any(|field| {
        let lowered = field.to_ascii_lowercase();
        fragments.iter().any(|fragment| lowered.contains(fragment))
    }) {
        return true;
    }
    fragments.iter().any(|fragment| combined.contains(fragment))
}

/// Strips common code prefixes so `429_insufficient_quota`-style codes still
/// match the quota table.
fn normalize_code_prefix(code: &str) -> &str {
    match code.split_once('_') {
        Some((prefix, rest))
            if prefix.chars().all(|character| character.is_ascii_digit()) && !rest.is_empty() =>
        {
            rest
        }
        _ => code,
    }
}
