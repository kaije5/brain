#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    AlreadyDeleted,
    RevisionOverflow,
    Validation { field: &'static str, reason: String },
}

impl DomainError {
    #[must_use]
    pub fn validation(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Validation {
            field,
            reason: reason.into(),
        }
    }
}
