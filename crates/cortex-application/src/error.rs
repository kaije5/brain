use cortex_domain::DomainError;

use crate::PolicyDeny;

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
    InferenceUnavailable,
    InferenceTimeout,
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
