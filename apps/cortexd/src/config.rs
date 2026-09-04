use std::{
    fs,
    path::{Path, PathBuf},
};

use cortex_application::{Capability, CapabilityCatalog, SecretRef};
use cortex_domain::{PrincipalId, WorkspaceId};
use ed25519_dalek::{SigningKey, VerifyingKey};

/// Configuration owned locally by the daemon process, never by an IPC caller.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) endpoint_name: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) principal_id: PrincipalId,
    pub(crate) inference_secret: Option<SecretRef>,
    pub(crate) pairing_verifier: VerifyingKey,
    pub(crate) pairing_signer: SigningKey,
    pub(crate) pairing_key_path: PathBuf,
    pub(crate) discovery_path: PathBuf,
    pub(crate) bootstrap_grants: Vec<Capability>,
}

impl DaemonConfig {
    /// Creates deterministic test-only local configuration with fresh ownership IDs.
    #[must_use]
    pub fn for_test(directory: &Path) -> Self {
        Self::with_fresh_pairing(
            directory.join("cortex.db"),
            format!("cortexd-test-{}", uuid::Uuid::now_v7()),
            WorkspaceId::new(),
            PrincipalId::new(),
            directory.join("cortexd-discovery.json"),
        )
    }

    fn with_fresh_pairing(
        database_path: PathBuf,
        endpoint_name: String,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        discovery_path: PathBuf,
    ) -> Self {
        let signer = fresh_signing_key();
        Self {
            database_path,
            endpoint_name,
            workspace_id,
            principal_id,
            inference_secret: None,
            pairing_verifier: signer.verifying_key(),
            pairing_signer: signer,
            pairing_key_path: pairing_key_path(&discovery_path),
            discovery_path,
            bootstrap_grants: CapabilityCatalog::all().to_vec(),
        }
    }

    /// Loads a daemon configuration without accepting any identity from a client.
    ///
    /// # Errors
    /// Returns a safe configuration error when the database path is absent.
    pub fn from_database_path(database_path: PathBuf) -> Result<Self, crate::DaemonError> {
        if database_path.as_os_str().is_empty() {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let discovery_path = database_path.with_extension("cortexd-discovery.json");
        let pairing_key_path = pairing_key_path(&discovery_path);
        if discovery_path.exists() {
            let bytes =
                fs::read(&discovery_path).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
            let discovery: Discovery = serde_json::from_slice(&bytes)
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
            let pairing_signer = load_pairing_signer(&pairing_key_path)?;
            let pairing_verifier = VerifyingKey::from_bytes(&discovery.pairing_verifier)
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
            if pairing_signer.verifying_key() != pairing_verifier {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
            return Ok(Self {
                database_path,
                endpoint_name: discovery.endpoint_name,
                workspace_id: WorkspaceId::try_from(discovery.workspace_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                principal_id: PrincipalId::try_from(discovery.principal_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                inference_secret: None,
                pairing_verifier,
                pairing_signer,
                pairing_key_path,
                discovery_path,
                bootstrap_grants: CapabilityCatalog::all().to_vec(),
            });
        }
        let config = Self::with_fresh_pairing(
            database_path,
            format!("cortexd-{}", uuid::Uuid::now_v7()),
            WorkspaceId::new(),
            PrincipalId::new(),
            discovery_path,
        );
        config.write_pairing_key()?;
        config.write_discovery()?;
        Ok(config)
    }

    /// Adds an opaque inference credential locator that must be resolved by the composition root.
    #[must_use]
    pub fn with_inference_secret(mut self, reference: SecretRef) -> Self {
        self.inference_secret = Some(reference);
        self
    }

    /// Narrows bootstrap grants for an isolated daemon instance. Production configuration keeps
    /// the explicit complete owner grant set unless an owner administration flow changes it.
    #[must_use]
    pub fn with_bootstrap_grants(mut self, grants: Vec<Capability>) -> Self {
        self.bootstrap_grants = grants;
        self
    }

    /// Opens the separate, per-user pairing enrollment artifact for a local client. The private
    /// key is never part of the discovery record or an IPC request.
    #[must_use]
    pub fn provisioned_client(&self) -> crate::ProvisionedLocalClient {
        crate::ProvisionedLocalClient::new(self.pairing_signer.clone())
    }

    pub(crate) fn write_discovery(&self) -> Result<(), crate::DaemonError> {
        let discovery = Discovery {
            endpoint_name: self.endpoint_name.clone(),
            workspace_id: self.workspace_id.into(),
            principal_id: self.principal_id.into(),
            pairing_verifier: self.pairing_verifier.to_bytes(),
        };
        let bytes =
            serde_json::to_vec(&discovery).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        fs::write(&self.discovery_path, bytes).map_err(|_| crate::DaemonError::InvalidConfiguration)
    }

    pub(crate) fn ensure_pairing_key(&self) -> Result<(), crate::DaemonError> {
        if self.pairing_key_path.exists() {
            let signer = load_pairing_signer(&self.pairing_key_path)?;
            return (signer.verifying_key() == self.pairing_verifier)
                .then_some(())
                .ok_or(crate::DaemonError::InvalidConfiguration);
        }
        self.write_pairing_key()
    }

    fn write_pairing_key(&self) -> Result<(), crate::DaemonError> {
        use std::io::Write;
        #[cfg(windows)]
        let mut file =
            match crate::windows_security::create_current_user_file(&self.pairing_key_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return self.ensure_pairing_key();
                }
                Err(_) => return Err(crate::DaemonError::InvalidConfiguration),
            };
        #[cfg(not(windows))]
        let mut file = {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.pairing_key_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return self.ensure_pairing_key();
                }
                Err(_) => return Err(crate::DaemonError::InvalidConfiguration),
            }
        };
        file.write_all(&self.pairing_signer.to_bytes())
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        file.sync_all()
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        Ok(())
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Discovery {
    endpoint_name: String,
    workspace_id: uuid::Uuid,
    principal_id: uuid::Uuid,
    pairing_verifier: [u8; 32],
}

pub(crate) struct IpcEnrollment {
    pub endpoint_name: String,
    pub principal_id: PrincipalId,
    pub signer: SigningKey,
}

pub(crate) fn load_ipc_enrollment(
    database_path: &Path,
) -> Result<IpcEnrollment, crate::DaemonError> {
    if database_path.as_os_str().is_empty() {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    let discovery_path = database_path.with_extension("cortexd-discovery.json");
    let discovery: Discovery = serde_json::from_slice(
        &fs::read(&discovery_path).map_err(|_| crate::DaemonError::InvalidConfiguration)?,
    )
    .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    let signer = load_pairing_signer(&pairing_key_path(&discovery_path))?;
    let verifier = VerifyingKey::from_bytes(&discovery.pairing_verifier)
        .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    if signer.verifying_key() != verifier {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    Ok(IpcEnrollment {
        endpoint_name: discovery.endpoint_name,
        principal_id: PrincipalId::try_from(discovery.principal_id)
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
        signer,
    })
}

fn fresh_signing_key() -> SigningKey {
    let mut seed = [0_u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::now_v7().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::now_v7().as_bytes());
    SigningKey::from_bytes(&seed)
}

fn pairing_key_path(discovery_path: &Path) -> PathBuf {
    discovery_path.with_extension("cortexd-pairing")
}

fn load_pairing_signer(path: &Path) -> Result<SigningKey, crate::DaemonError> {
    let bytes = fs::read(path).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    Ok(SigningKey::from_bytes(&seed))
}
