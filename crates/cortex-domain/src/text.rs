//! Shared bounded-text validation for domain values.

use crate::DomainError;

pub(crate) fn validate_text(field: &'static str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::validation(field, "must not be blank"));
    }
    Ok(())
}
