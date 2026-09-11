//! SCRUM-84 acceptance tests: typed provider error taxonomy, Retry-After
//! parsing, sanitization, and hint-driven consumer recovery.

use std::time::Duration;

use cortex_application::{ApplicationError, RecoveryHint};
use cortex_inference::{
    ProviderError, ProviderFailureCategory, classify_http_response, map_provider_error,
    parse_retry_after,
};

fn header(value: &str) -> reqwest::header::HeaderValue {
    reqwest::header::HeaderValue::from_str(value).expect("static header value")
}

fn classify(
    status: u16,
    retry_after: Option<&reqwest::header::HeaderValue>,
    body: &str,
) -> ProviderError {
    classify_http_response(status, retry_after, body.as_bytes())
}

#[test]
fn every_category_is_representable_with_the_documented_recovery_hint() {
    let retry_same = [
        ProviderFailureCategory::Timeout,
        ProviderFailureCategory::Unavailable,
        ProviderFailureCategory::RateLimit,
        ProviderFailureCategory::Overloaded,
        ProviderFailureCategory::ServerError,
    ];
    for category in retry_same {
        assert_eq!(category.recovery_hint(), RecoveryHint::RetrySameSelection);
        assert!(category.retryable());
    }
    assert_eq!(
        ProviderFailureCategory::ContextOverflow.recovery_hint(),
        RecoveryHint::CompactContext
    );
    assert!(!ProviderFailureCategory::ContextOverflow.retryable());
    for category in [
        ProviderFailureCategory::Auth,
        ProviderFailureCategory::QuotaOrBilling,
        ProviderFailureCategory::InvalidRequest,
        ProviderFailureCategory::MalformedResponse,
    ] {
        assert_eq!(category.recovery_hint(), RecoveryHint::Abort);
        assert!(!category.retryable());
    }
}

#[test]
fn http_statuses_map_to_the_documented_categories() {
    let table: [(u16, &str, ProviderFailureCategory); 7] = [
        (401, "{}", ProviderFailureCategory::Auth),
        (403, "{}", ProviderFailureCategory::Auth),
        (404, "{}", ProviderFailureCategory::InvalidRequest),
        (422, "{}", ProviderFailureCategory::InvalidRequest),
        (500, "{}", ProviderFailureCategory::ServerError),
        (502, "{}", ProviderFailureCategory::ServerError),
        (503, "{}", ProviderFailureCategory::ServerError),
    ];
    for (status, body, expected) in table {
        let classified = classify(status, None, body);
        assert_eq!(classified.category(), expected, "status {status}");
        assert_eq!(classified.status(), Some(status));
    }
}

#[test]
fn identical_429s_classify_differently_based_on_structured_codes() {
    let throttled = classify(
        429,
        None,
        r#"{"error": {"message": "too many requests", "type": "rate_limit_error", "code": "rate_limit_exceeded"}}"#,
    );
    assert_eq!(throttled.category(), ProviderFailureCategory::RateLimit);

    // The same status with an insufficient-quota structured code is billing
    // exhaustion and must never be retried by SCRUM-85.
    let quota_by_code = classify(
        429,
        None,
        r#"{"error": {"message": "check your plan and billing details", "type": "insufficient_quota"}}"#,
    );
    assert_eq!(
        quota_by_code.category(),
        ProviderFailureCategory::QuotaOrBilling
    );
    assert_eq!(quota_by_code.status(), Some(429));

    let quota_by_code_string = classify(
        429,
        None,
        r#"{"error": {"message": "quota exceeded for this project", "code": "insufficient_quota"}}"#,
    );
    assert_eq!(
        quota_by_code_string.category(),
        ProviderFailureCategory::QuotaOrBilling
    );
}

#[test]
fn structured_overload_signals_classify_as_overloaded() {
    let overloaded = classify(
        503,
        None,
        r#"{"error": {"message": "the model is overloaded", "type": "overloaded_error"}}"#,
    );
    assert_eq!(overloaded.category(), ProviderFailureCategory::Overloaded);
    assert!(overloaded.category().retryable());
}

#[test]
fn context_overflow_is_first_class_and_distinct_from_generic_400() {
    let structured = classify(
        400,
        None,
        r#"{"error": {"message": "request too large", "code": "context_length_exceeded"}}"#,
    );
    assert_eq!(
        structured.category(),
        ProviderFailureCategory::ContextOverflow
    );

    // Message-fragment fallback stays isolated inside the classifier and is
    // covered here so SCRUM-79 can consume the typed category without
    // parsing text.
    let fragmented = classify(
        400,
        None,
        r#"{"error": {"message": "This model's maximum context length is 4096 tokens"}}"#,
    );
    assert_eq!(
        fragmented.category(),
        ProviderFailureCategory::ContextOverflow
    );

    let generic = classify(
        400,
        None,
        r#"{"error": {"message": "invalid value for temperature", "code": "invalid_parameter"}}"#,
    );
    assert_eq!(generic.category(), ProviderFailureCategory::InvalidRequest);
    assert_eq!(
        map_provider_error(&generic),
        ApplicationError::InvalidInferenceRequest
    );
}

#[test]
fn retry_after_is_parsed_from_delta_seconds_and_http_dates() {
    assert_eq!(
        parse_retry_after(Some(&header("120"))),
        Some(Duration::from_mins(2))
    );
    assert_eq!(parse_retry_after(Some(&header("0"))), Some(Duration::ZERO));
    // Future HTTP date yields a positive delay; a past date clamps to zero.
    let future = chrono::Utc::now() + chrono::Duration::seconds(60);
    let parsed = parse_retry_after(Some(&header(
        &future.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
    )));
    assert!(parsed.is_some_and(|delay| delay <= Duration::from_mins(1)));
    let past = chrono::Utc::now() - chrono::Duration::seconds(60);
    assert_eq!(
        parse_retry_after(Some(&header(
            &past.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
        ))),
        Some(Duration::ZERO)
    );
    // Invalid and absent headers never panic.
    assert_eq!(parse_retry_after(Some(&header("soon, probably"))), None);
    assert_eq!(parse_retry_after(Some(&header(""))), None);
    assert_eq!(parse_retry_after(None), None);
}

#[test]
fn rate_limit_retry_after_survives_the_application_mapping() {
    let classified = classify(
        429,
        Some(&header("45")),
        r#"{"error": {"message": "slow down", "code": "rate_limit_exceeded"}}"#,
    );
    assert_eq!(classified.retry_after(), Some(Duration::from_secs(45)));
    assert_eq!(
        map_provider_error(&classified),
        ApplicationError::RateLimited {
            retry_after_secs: Some(45)
        }
    );
}

#[test]
fn formatted_errors_are_sanitized_and_never_leak_credentials() {
    const SENTINEL_KEY: &str = "nvapi-super-secret-key-1234567890";
    const SENTINEL_BEARER: &str = "Bearer sk-plainly-visible-token";

    let leaked = classify(
        401,
        None,
        &format!(
            r#"{{"error": {{"message": "invalid key {SENTINEL_KEY} via {SENTINEL_BEARER} Authorization header rejected, and more follow-up detail text to push the message past the truncation boundary for good measure so truncation is exercised too"}}}}"#
        ),
    );
    let rendered = leaked.to_string();
    assert!(!rendered.contains(SENTINEL_KEY), "leaked key: {rendered}");
    assert!(!rendered.contains("sk-"), "leaked bearer: {rendered}");
    assert!(rendered.contains("<redacted>"), "redaction marker expected");
    assert!(rendered.len() < 512, "formatted errors stay bounded");
    assert_eq!(leaked.category(), ProviderFailureCategory::Auth);
    assert_eq!(leaked.provider_code(), None);

    // Non-JSON error bodies carry no message at all (no raw body leakage).
    let raw_body = classify(
        500,
        None,
        "HTTP 500 Internal Server Error with secret-key-abc123",
    );
    assert!(raw_body.message().is_empty());
}

#[test]
fn application_error_recovery_hints_are_typed_not_string_based() {
    let table = [
        (
            ApplicationError::InferenceTimeout,
            RecoveryHint::RetrySameSelection,
        ),
        (
            ApplicationError::InferenceUnavailable,
            RecoveryHint::RetrySameSelection,
        ),
        (
            ApplicationError::RateLimited {
                retry_after_secs: Some(30),
            },
            RecoveryHint::RetrySameSelection,
        ),
        (
            ApplicationError::ContextOverflow,
            RecoveryHint::CompactContext,
        ),
        (ApplicationError::AuthenticationFailed, RecoveryHint::Abort),
        (ApplicationError::QuotaExceeded, RecoveryHint::Abort),
        (
            ApplicationError::InvalidInferenceRequest,
            RecoveryHint::Abort,
        ),
        (ApplicationError::NoSuitableModel, RecoveryHint::Abort),
    ];
    for (error, hint) in table {
        assert_eq!(error.recovery_hint(), hint, "{error:?}");
    }
}

#[test]
fn quota_and_auth_failures_map_to_non_retryable_application_errors() {
    for (status, body) in [
        (401, r#"{"error": {"message": "bad key"}}"#),
        (
            429,
            r#"{"error": {"message": "quota", "code": "insufficient_quota"}}"#,
        ),
        (400, r#"{"error": {"message": "unknown model"}}"#),
    ] {
        let mapped = map_provider_error(&classify(status, None, body));
        assert_eq!(mapped.recovery_hint(), RecoveryHint::Abort, "{mapped:?}");
    }
}

#[test]
fn retryable_categories_map_to_retry_same_selection_application_errors() {
    for (status, body) in [
        (503, r#"{"error": {"message": "unavailable"}}"#),
        (500, r#"{"error": {"message": "boom"}}"#),
    ] {
        let mapped = map_provider_error(&classify(status, None, body));
        assert_eq!(
            mapped.recovery_hint(),
            RecoveryHint::RetrySameSelection,
            "{mapped:?}"
        );
    }
    let timeout = map_provider_error(&ProviderError::from_category(
        ProviderFailureCategory::Timeout,
    ));
    assert_eq!(timeout, ApplicationError::InferenceTimeout);
}
