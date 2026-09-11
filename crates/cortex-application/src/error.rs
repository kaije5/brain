use cortex_domain::DomainError;

use crate::PolicyDeny;

/// Deterministic recovery policy signal derived from a typed failure
/// category. Consumers branch on this hint instead of parsing error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryHint {
    /// The same resolved model/provider selection may be retried after the
    /// failure (timeout, transport unavailability, throttling, overload,
    /// transient server error). Never a different selection.
    RetrySameSelection,
    /// The request exceeded the selected model's context window: compact the
    /// context and retry when supported, otherwise abort.
    CompactContext,
    /// The failure is permanent for the current configuration or credential;
    /// retrying the same selection cannot succeed.
    Abort,
}

/// Safe public failures returned by the application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationError {
    Validation {
        field: &'static str,
    },
    NotFound {
        entity: &'static str,
    },
    Conflict {
        entity: &'static str,
    },
    PermissionDenied,
    PolicyDenied(PolicyDeny),
    Storage(String),
    /// The OS credential service is absent, locked, or inaccessible. There is
    /// no plaintext fallback: the operator must unlock or configure the
    /// platform keyring.
    SecretStoreUnavailable,
    InferenceUnavailable,
    InferenceTimeout,
    /// The provider throttled the request (HTTP 429 without a quota/billing
    /// structured code). `retry_after_secs` carries a parsed `Retry-After`
    /// when the provider supplied one.
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    /// The credential was rejected as invalid, expired, or unauthorized.
    AuthenticationFailed,
    /// Insufficient quota, credits, or billing status at the provider.
    QuotaExceeded,
    /// The request exceeded the selected model's context window.
    ContextOverflow,
    /// The request was rejected as unsupported or malformed by configuration
    /// (unknown model, unsupported parameter, other caller-side 4xx).
    InvalidInferenceRequest,
    /// Degraded routing state: no enabled provider exposes a model with fresh
    /// evidence for the role's required capabilities. Never silently falls
    /// back to a substitute model or provider.
    NoSuitableModel,
    MalformedModelOutput {
        reason: &'static str,
    },
    Internal,
}

impl From<DomainError> for ApplicationError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::AlreadyDeleted | DomainError::RevisionOverflow => {
                Self::Conflict { entity: "entity" }
            }
            DomainError::Validation { field, .. } => Self::Validation { field },
        }
    }
}

impl ApplicationError {
    /// Deterministic recovery policy for this failure. Consumers branch on
    /// this hint rather than matching error strings; retries may only target
    /// the same resolved selection (ADR-024).
    #[must_use]
    pub const fn recovery_hint(&self) -> RecoveryHint {
        match self {
            Self::InferenceTimeout | Self::InferenceUnavailable | Self::RateLimited { .. } => {
                RecoveryHint::RetrySameSelection
            }
            Self::ContextOverflow => RecoveryHint::CompactContext,
            Self::Validation { .. }
            | Self::NotFound { .. }
            | Self::Conflict { .. }
            | Self::PermissionDenied
            | Self::PolicyDenied(_)
            | Self::Storage(_)
            | Self::SecretStoreUnavailable
            | Self::AuthenticationFailed
            | Self::QuotaExceeded
            | Self::InvalidInferenceRequest
            | Self::NoSuitableModel
            | Self::MalformedModelOutput { .. }
            | Self::Internal => RecoveryHint::Abort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApplicationError, RecoveryHint};

    #[test]
    fn unavailable_secret_store_requires_operator_action() {
        assert_eq!(
            ApplicationError::SecretStoreUnavailable.recovery_hint(),
            RecoveryHint::Abort
        );
    }
}
