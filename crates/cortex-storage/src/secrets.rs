use std::fmt;

use cortex_application::ApplicationError;

/// Opaque locator for credential material held by a platform secret store.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SecretRef(String);

impl SecretRef {
    /// Validates a platform secret reference without resolving secret material.
    ///
    /// # Errors
    ///
    /// Returns a validation error for blank, oversized, or control-character references.
    pub fn new(value: impl Into<String>) -> Result<Self, ApplicationError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(ApplicationError::Validation {
                field: "secret_ref",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef([REDACTED])")
    }
}

/// Platform boundary that canonicalizes opaque secret references without
/// returning credential values to storage, application, model, or transport code.
#[allow(async_fn_in_trait)]
pub trait SecretStore: Send + Sync {
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretRef, ApplicationError>;
}
