use cortex_application::{ApplicationError, SecretRef, SecretStore};
#[cfg(windows)]
use keyring_core::api::CredentialStoreApi;
#[cfg(windows)]
use zeroize::Zeroize;

/// Process composition adapter for opaque references held in the current user's platform store.
/// It verifies that referenced material exists without retaining or returning the credential value.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformSecretStore;

impl SecretStore for PlatformSecretStore {
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretRef, ApplicationError> {
        #[cfg(windows)]
        {
            let (service, username) = keyring_target(reference)?;
            let store = windows_native_keyring_store::Store::new()
                .map_err(|_| ApplicationError::Internal)?;
            let entry = store
                .build(service, username, None)
                .map_err(|_| ApplicationError::Internal)?;
            let mut credential = entry.get_secret().map_err(|_| ApplicationError::Internal)?;
            credential.zeroize();
            Ok(reference.clone())
        }

        #[cfg(not(windows))]
        {
            let _ = reference;
            Err(ApplicationError::Internal)
        }
    }
}

#[cfg(windows)]
fn keyring_target(reference: &SecretRef) -> Result<(&str, &str), ApplicationError> {
    let target = reference
        .as_str()
        .strip_prefix("keyring:")
        .and_then(|value| value.split_once('/'));
    match target {
        Some((service, username)) if !service.is_empty() && !username.is_empty() => {
            Ok((service, username))
        }
        _ => Err(ApplicationError::Validation {
            field: "secret_ref",
        }),
    }
}

impl PlatformSecretStore {
    /// Resolves the raw credential value for one reference. This is the
    /// composition-root exception to the `SecretStore` contract: the value is
    /// returned (zeroized on drop) so the daemon can authenticate provider
    /// transports, and it must never be logged, serialized, or stored.
    ///
    /// # Errors
    /// Returns a redacted error when the platform store rejects the read.
    pub fn resolve_value(
        &self,
        reference: &SecretRef,
    ) -> Result<zeroize::Zeroizing<String>, ApplicationError> {
        #[cfg(windows)]
        {
            let (service, username) = keyring_target(reference)?;
            let store = windows_native_keyring_store::Store::new()
                .map_err(|_| ApplicationError::Internal)?;
            let entry = store
                .build(service, username, None)
                .map_err(|_| ApplicationError::Internal)?;
            let credential = entry.get_secret().map_err(|_| ApplicationError::Internal)?;
            Ok(zeroize::Zeroizing::new(
                String::from_utf8_lossy(&credential).into_owned(),
            ))
        }

        #[cfg(not(windows))]
        {
            let _ = reference;
            Err(ApplicationError::Internal)
        }
    }
}
