use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub use cortexd::{
    data_directory, data_directory_for, default_database_path, default_database_path_for,
};

const KEYRING_SERVICE: &str = "cortexd";
const MAX_PROFILE_LEN: usize = 64;

/// Redacted failure category for local (non-daemon) client operations.
/// Errors never embed raw secret material.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LocalOpError {
    #[error("config file already exists")]
    ConfigAlreadyExists,
    #[error("config file could not be written")]
    ConfigWrite,
    #[error("invalid profile identifier")]
    InvalidProfile,
    #[error("empty secret")]
    MissingSecret,
    #[error("platform secret store is unavailable")]
    StoreUnavailable,
}

/// Write boundary for platform keyring material, so imports are testable
/// without touching the real OS store.
pub trait SecretWriter {
    /// # Errors
    /// Returns [`LocalOpError::StoreUnavailable`] when the platform store
    /// rejects the write.
    fn write(&self, service: &str, username: &str, secret: &[u8]) -> Result<(), LocalOpError>;
}

/// Writes the documented `cortexd.toml` template beside the database without
/// ever overwriting an existing configuration.
///
/// # Errors
/// Returns [`LocalOpError::ConfigAlreadyExists`] when the file exists, or
/// [`LocalOpError::ConfigWrite`] when the filesystem rejects the write.
pub fn init_config(database_path: &Path) -> Result<PathBuf, LocalOpError> {
    let directory = database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(cortexd::data_directory, Path::to_path_buf);
    let path = directory.join("cortexd.toml");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            LocalOpError::ConfigAlreadyExists
        } else {
            LocalOpError::ConfigWrite
        }
    })?;
    file.write_all(cortexd::LocalSettings::template().as_bytes())
        .map_err(|_| LocalOpError::ConfigWrite)?;
    Ok(path)
}

/// Imports a provider credential into the platform keyring under
/// `cortexd/<profile>` and returns the non-secret `SecretRef` locator that
/// `cortexd.toml` model profiles reference.
///
/// # Errors
/// Returns [`LocalOpError::InvalidProfile`] for profile ids that could not
/// form a safe keyring target, [`LocalOpError::MissingSecret`] for empty
/// secrets, or [`LocalOpError::StoreUnavailable`] when the platform store
/// rejects the write.
pub fn import_secret<W: SecretWriter + ?Sized>(
    store: &W,
    profile: &str,
    secret: &[u8],
) -> Result<String, LocalOpError> {
    validate_profile(profile)?;
    if secret.is_empty() {
        return Err(LocalOpError::MissingSecret);
    }
    store.write(KEYRING_SERVICE, profile, secret)?;
    Ok(format!("keyring:{KEYRING_SERVICE}/{profile}"))
}

/// Validates a provider profile identifier for use in `cortexd.toml` and as
/// a keyring username.
///
/// # Errors
/// Returns a human-readable rejection reason.
pub fn validate_profile_id(profile: &str) -> Result<(), String> {
    validate_profile(profile).map_err(|error| error.to_string())
}

fn validate_profile(profile: &str) -> Result<(), LocalOpError> {
    let valid = !profile.is_empty()
        && profile.len() <= MAX_PROFILE_LEN
        && profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-');
    if valid {
        Ok(())
    } else {
        Err(LocalOpError::InvalidProfile)
    }
}

/// Writes into the current user's platform secret store (Windows Credential
/// Manager, macOS Keychain, or a Linux Secret Service-compatible backend),
/// matching `PlatformSecretStore` resolution. Absent or locked credential
/// services fail closed with [`LocalOpError::StoreUnavailable`]; there is no
/// plaintext fallback.
pub struct PlatformSecretWriter;

impl SecretWriter for PlatformSecretWriter {
    fn write(&self, service: &str, username: &str, secret: &[u8]) -> Result<(), LocalOpError> {
        use zeroize::Zeroize;
        let store = cortex_keyring::platform_store().map_err(|_| LocalOpError::StoreUnavailable)?;
        let mut material = secret.to_vec();
        let result = cortex_keyring::write_secret(store.as_ref(), service, username, &material);
        material.zeroize();
        result.map_err(|_| LocalOpError::StoreUnavailable)
    }
}
