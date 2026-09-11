#![forbid(unsafe_code)]

//! Cross-platform OS keyring adapter behind the `SecretStore` contract
//! (SCRUM-81). Every platform-specific credential store stays inside this
//! crate; domain and application layers depend only on `SecretRef` /
//! `SecretStore`. Secret values and raw OS error payloads never appear in
//! errors: failures carry a typed, actionable category only.

use cortex_application::{ApplicationError, SecretRef};
use keyring_core::api::CredentialStoreApi;

/// Typed, actionable keyring failure categories. Deliberately payload-free:
/// raw OS keyring errors can embed provider-side detail and must never reach
/// logs or IPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KeyringFailure {
    /// The `SecretRef` is not a well-formed `keyring:service/username` locator.
    #[error("invalid keyring secret reference (expected keyring:service/username)")]
    InvalidReference,
    /// The OS credential service is absent, locked, or inaccessible.
    #[error(
        "OS keyring is unavailable; unlock or configure the platform credential service (no plaintext fallback exists)"
    )]
    Unavailable,
    /// No credential exists for the referenced service and account.
    #[error("no credential exists for the referenced service and account in the OS keyring")]
    NotFound,
}

impl From<KeyringFailure> for ApplicationError {
    fn from(failure: KeyringFailure) -> Self {
        match failure {
            KeyringFailure::InvalidReference => ApplicationError::Validation {
                field: "secret_ref",
            },
            KeyringFailure::Unavailable | KeyringFailure::NotFound => {
                ApplicationError::SecretStoreUnavailable
            }
        }
    }
}

/// Maps a `keyring_core` error onto a payload-free failure category. The raw
/// error (and any attached platform payload) is dropped here, on purpose.
fn classify(error: keyring_core::Error) -> KeyringFailure {
    match error {
        keyring_core::Error::NoEntry => KeyringFailure::NotFound,
        _ => KeyringFailure::Unavailable,
    }
}

/// Parses the deterministic `keyring:service/username` locator into its
/// keyring target. This mapping is the compatibility contract: credentials
/// written by earlier cortex versions resolve at exactly the same
/// service/account pair.
///
/// # Errors
/// Returns [`KeyringFailure::InvalidReference`] for any other shape.
pub fn keyring_target(reference: &SecretRef) -> Result<(&str, &str), KeyringFailure> {
    let target = reference
        .as_str()
        .strip_prefix("keyring:")
        .and_then(|value| value.split_once('/'));
    match target {
        Some((service, username)) if !service.is_empty() && !username.is_empty() => {
            Ok((service, username))
        }
        _ => Err(KeyringFailure::InvalidReference),
    }
}

/// Reads the secret bytes for one keyring target. The caller owns the
/// returned buffer and must zeroize it after use.
///
/// # Errors
/// Returns payload-free failures for invalid targets, missing credentials,
/// and unavailable credential services.
pub fn read_secret(
    store: &dyn CredentialStoreApi,
    service: &str,
    username: &str,
) -> Result<Vec<u8>, KeyringFailure> {
    let entry = store.build(service, username, None).map_err(classify)?;
    entry.get_secret().map_err(classify)
}

/// Writes (or replaces) the secret bytes for one keyring target.
///
/// # Errors
/// Returns payload-free failures for invalid targets and unavailable
/// credential services.
pub fn write_secret(
    store: &dyn CredentialStoreApi,
    service: &str,
    username: &str,
    secret: &[u8],
) -> Result<(), KeyringFailure> {
    let entry = store.build(service, username, None).map_err(classify)?;
    entry.set_secret(secret).map_err(classify)
}

/// The current platform's native credential store: Windows Credential
/// Manager, macOS Keychain, or a Linux Secret Service-compatible backend.
///
/// # Errors
/// Returns [`KeyringFailure::Unavailable`] when the platform credential
/// service cannot be reached. There is no plaintext or environment fallback.
pub fn platform_store() -> Result<std::sync::Arc<dyn CredentialStoreApi>, KeyringFailure> {
    platform_store_impl()
}

#[cfg(windows)]
fn platform_store_impl() -> Result<std::sync::Arc<dyn CredentialStoreApi>, KeyringFailure> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as std::sync::Arc<dyn CredentialStoreApi>)
        .map_err(|_| KeyringFailure::Unavailable)
}

#[cfg(target_os = "macos")]
fn platform_store_impl() -> Result<std::sync::Arc<dyn CredentialStoreApi>, KeyringFailure> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as std::sync::Arc<dyn CredentialStoreApi>)
        .map_err(|_| KeyringFailure::Unavailable)
}

#[cfg(target_os = "linux")]
fn platform_store_impl() -> Result<std::sync::Arc<dyn CredentialStoreApi>, KeyringFailure> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as std::sync::Arc<dyn CredentialStoreApi>)
        .map_err(|_| KeyringFailure::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroize;

    fn reference(value: &str) -> SecretRef {
        SecretRef::new(value).expect("valid secret reference")
    }

    #[test]
    fn keyring_targets_map_deterministically() {
        let secret_reference = reference("keyring:cortexd/nim");
        let (service, username) = keyring_target(&secret_reference).expect("valid target");
        assert_eq!(service, "cortexd");
        assert_eq!(username, "nim");
    }

    #[test]
    fn malformed_references_are_rejected_without_panicking() {
        for value in [
            "file:///etc/passwd",
            "keyring:noseparator",
            "keyring:/username",
            "keyring:service/",
            "keyring:",
        ] {
            let secret_reference = reference(value);
            assert_eq!(
                keyring_target(&secret_reference).expect_err(value),
                KeyringFailure::InvalidReference,
            );
        }
    }

    #[test]
    fn mock_store_round_trips_secrets_at_the_same_target() {
        let store = keyring_core::mock::Store::new().expect("mock store");
        write_secret(store.as_ref(), "cortexd", "nim", b"sentinel-value").expect("write");
        let mut secret = read_secret(store.as_ref(), "cortexd", "nim").expect("read");
        assert_eq!(secret, b"sentinel-value");
        secret.zeroize();
        assert!(secret.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn missing_credentials_classify_as_not_found() {
        let store = keyring_core::mock::Store::new().expect("mock store");
        assert_eq!(
            read_secret(store.as_ref(), "cortexd", "absent").expect_err("absent"),
            KeyringFailure::NotFound
        );
    }

    #[test]
    fn failures_never_carry_secret_payloads() {
        // A failure formatted anywhere must never contain the secret value.
        const SENTINEL: &str = "nvapi-sentinel-secret-value";
        let failure = KeyringFailure::Unavailable;
        let formatted = format!("{failure}");
        assert!(!formatted.contains(SENTINEL));
        let mapped: ApplicationError = failure.into();
        let mapped_formatted = format!("{mapped:?}");
        assert!(!mapped_formatted.contains(SENTINEL));
    }
}
