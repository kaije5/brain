use cortex_application::{ApplicationError, SecretRef, SecretStore};
use cortex_keyring::{keyring_target, platform_store, read_secret};
use zeroize::Zeroize;

/// Process composition adapter for opaque references held in the current
/// user's platform credential store (Windows Credential Manager, macOS
/// Keychain, or a Linux Secret Service-compatible backend). It verifies that
/// referenced material exists without retaining or returning the value.
/// Missing, locked, or absent credential services surface as the typed
/// [`ApplicationError::SecretStoreUnavailable`]; there is no plaintext
/// fallback.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformSecretStore;

impl SecretStore for PlatformSecretStore {
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretRef, ApplicationError> {
        let (service, username) = keyring_target(reference).map_err(ApplicationError::from)?;
        let store = platform_store().map_err(ApplicationError::from)?;
        let mut credential =
            read_secret(store.as_ref(), service, username).map_err(ApplicationError::from)?;
        credential.zeroize();
        Ok(reference.clone())
    }
}

impl PlatformSecretStore {
    /// Resolves the raw credential value for one reference. This is the
    /// composition-root exception to the `SecretStore` contract: the value is
    /// returned (zeroized on drop) so the daemon can authenticate provider
    /// transports, and it must never be logged, serialized, or stored.
    ///
    /// # Errors
    /// Returns a redacted, typed error when the platform store rejects the
    /// read or the reference is malformed.
    pub fn resolve_value(
        &self,
        reference: &SecretRef,
    ) -> Result<zeroize::Zeroizing<String>, ApplicationError> {
        let (service, username) = keyring_target(reference).map_err(ApplicationError::from)?;
        let store = platform_store().map_err(ApplicationError::from)?;
        let credential =
            read_secret(store.as_ref(), service, username).map_err(ApplicationError::from)?;
        Ok(zeroize::Zeroizing::new(
            String::from_utf8_lossy(&credential).into_owned(),
        ))
    }
}
