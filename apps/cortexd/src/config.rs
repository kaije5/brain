use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use cortex_application::{Capability, CapabilityCatalog, SecretRef};
use cortex_domain::{PrincipalId, WorkspaceId};
use cortex_inference::{OpenAiCompatibleConfig, ProviderLimits};
use ed25519_dalek::{SigningKey, VerifyingKey};

const MAX_REMOTE_CLIENTS: usize = 16;
const MODEL_RESPONSE_BYTES: usize = 64 * 1024;
const MODEL_EMBEDDING_INPUT_BYTES: usize = 32 * 1024;
const MODEL_EMBEDDING_DIMENSIONS: usize = 4096;

/// Configuration owned locally by the daemon process, never by an IPC caller.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) endpoint_name: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) principal_id: PrincipalId,
    pub(crate) inference_secret: Option<SecretRef>,
    pub(crate) model_config: Option<OpenAiCompatibleConfig>,
    pub(crate) pairing_verifier: VerifyingKey,
    pub(crate) pairing_signer: SigningKey,
    pub(crate) pairing_key_path: PathBuf,
    pub(crate) discovery_path: PathBuf,
    pub(crate) bootstrap_grants: Vec<Capability>,
    pub(crate) remote_clients: Vec<RemoteClientConfig>,
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
            model_config: None,
            pairing_verifier: signer.verifying_key(),
            pairing_signer: signer,
            pairing_key_path: pairing_key_path(&discovery_path),
            discovery_path,
            bootstrap_grants: CapabilityCatalog::all().to_vec(),
            remote_clients: Vec::new(),
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
            let remote_clients = load_remote_clients(&database_path)?;
            return Ok(Self {
                database_path,
                endpoint_name: discovery.endpoint_name,
                workspace_id: WorkspaceId::try_from(discovery.workspace_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                principal_id: PrincipalId::try_from(discovery.principal_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                inference_secret: None,
                model_config: None,
                pairing_verifier,
                pairing_signer,
                pairing_key_path,
                discovery_path,
                bootstrap_grants: CapabilityCatalog::all().to_vec(),
                remote_clients,
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

    /// Loads durable daemon identity plus the deployed loopback model settings from the process
    /// environment. `CORTEX_MODEL_BASE_URL` and `CORTEX_MODEL_NAME` must either both be absent or
    /// both be present. `CORTEX_MODEL_SECRET_REF` is optional, but is accepted only alongside the
    /// provider settings; it remains an opaque platform-store locator and is never a credential.
    ///
    /// # Errors
    /// Returns a redacted configuration error for incomplete, non-Unicode, or invalid settings.
    pub fn from_environment(database_path: PathBuf) -> Result<Self, crate::DaemonError> {
        let config = Self::from_database_path(database_path)?;
        let endpoint = environment_value("CORTEX_MODEL_BASE_URL")?;
        let model = environment_value("CORTEX_MODEL_NAME")?;
        let secret = environment_value("CORTEX_MODEL_SECRET_REF")?
            .map(SecretRef::new)
            .transpose()
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        match (endpoint, model, secret) {
            (None, None, None) => Ok(config),
            (Some(endpoint), Some(model), secret) => {
                let limits = ProviderLimits::new(
                    MODEL_RESPONSE_BYTES,
                    MODEL_EMBEDDING_INPUT_BYTES,
                    MODEL_EMBEDDING_DIMENSIONS,
                )
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
                let provider = OpenAiCompatibleConfig::new(
                    endpoint,
                    model,
                    secret,
                    Duration::from_secs(5),
                    limits,
                )
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
                Ok(config.with_model_config(provider))
            }
            _ => Err(crate::DaemonError::InvalidConfiguration),
        }
    }

    /// Adds an opaque inference credential locator that must be resolved by the composition root.
    #[must_use]
    pub fn with_inference_secret(mut self, reference: SecretRef) -> Self {
        self.inference_secret = Some(reference);
        self
    }

    /// Configures the bounded loopback OpenAI-compatible provider used by search and the agent.
    #[must_use]
    pub fn with_model_config(mut self, config: OpenAiCompatibleConfig) -> Self {
        self.model_config = Some(config);
        self
    }

    /// Narrows bootstrap grants for an isolated daemon instance. Production configuration keeps
    /// the explicit complete owner grant set unless an owner administration flow changes it.
    #[must_use]
    pub fn with_bootstrap_grants(mut self, grants: Vec<Capability>) -> Self {
        self.bootstrap_grants = grants;
        self
    }

    /// Returns the daemon owner identity for local administration and enrollment setup.
    #[must_use]
    pub fn owner_principal_id(&self) -> uuid::Uuid {
        self.principal_id.into()
    }

    /// Returns the durable workspace identity for local process composition and fixtures.
    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Creates a distinct, durable local IPC enrollment for a remote paired principal.
    /// Initial grants are inserted only when the principal is first created, so later
    /// revocations survive daemon restarts.
    ///
    /// # Errors
    /// Returns a redacted configuration error for duplicates, unsafe bounds, or persistence
    /// failures.
    pub fn enroll_remote_principal(
        &mut self,
        principal_id: PrincipalId,
        grants: &[Capability],
    ) -> Result<PathBuf, crate::DaemonError> {
        if principal_id == self.principal_id
            || grants.is_empty()
            || self.remote_clients.len() >= MAX_REMOTE_CLIENTS
            || self
                .remote_clients
                .iter()
                .any(|client| client.principal_id == principal_id)
        {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let signer = fresh_signing_key();
        let enrollment_path = remote_enrollment_path(&self.database_path, principal_id);
        let enrollment = PrivateEnrollment {
            endpoint_name: self.endpoint_name.clone(),
            principal_id: principal_id.into(),
            signing_key: signer.to_bytes(),
        };
        write_private_bytes(
            &enrollment_path,
            &serde_json::to_vec(&enrollment)
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
        )?;
        self.remote_clients.push(RemoteClientConfig {
            principal_id,
            pairing_verifier: signer.verifying_key(),
            bootstrap_grants: grants.to_vec(),
            subject: String::new(),
        });
        write_remote_clients(&self.database_path, &self.remote_clients)?;
        Ok(enrollment_path)
    }

    /// Creates or returns one durable remote identity for a validated OIDC subject.
    ///
    /// This is intentionally reachable only from the daemon's authenticated local-owner
    /// provisioning path. The result contains no key material; callers receive the protected
    /// enrollment file path needed by the loopback gateway configuration.
    ///
    /// # Errors
    /// Returns a redacted configuration error for an invalid subject/grant set or unavailable
    /// protected local storage.
    pub fn enroll_remote_subject(
        &mut self,
        subject: &str,
        grants: &[Capability],
    ) -> Result<RemoteEnrollment, crate::DaemonError> {
        validate_subject(subject)?;
        validate_remote_grants(grants)?;
        if let Some(client) = self
            .remote_clients
            .iter()
            .find(|client| client.subject == subject)
        {
            if client.bootstrap_grants != grants {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
            return Ok(RemoteEnrollment {
                principal_id: client.principal_id,
                enrollment_path: remote_enrollment_path(&self.database_path, client.principal_id),
                subject: subject.to_owned(),
                grants: grants.to_vec(),
            });
        }
        if self.remote_clients.len() >= MAX_REMOTE_CLIENTS {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let principal_id = PrincipalId::new();
        let signer = fresh_signing_key();
        let enrollment_path = remote_enrollment_path(&self.database_path, principal_id);
        let enrollment = PrivateEnrollment {
            endpoint_name: self.endpoint_name.clone(),
            principal_id: principal_id.into(),
            signing_key: signer.to_bytes(),
        };
        write_private_bytes(
            &enrollment_path,
            &serde_json::to_vec(&enrollment)
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
        )?;
        self.remote_clients.push(RemoteClientConfig {
            principal_id,
            pairing_verifier: signer.verifying_key(),
            bootstrap_grants: grants.to_vec(),
            subject: subject.to_owned(),
        });
        write_remote_clients(&self.database_path, &self.remote_clients)?;
        Ok(RemoteEnrollment {
            principal_id,
            enrollment_path,
            subject: subject.to_owned(),
            grants: grants.to_vec(),
        })
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
        write_private_bytes(&self.pairing_key_path, &self.pairing_signer.to_bytes())
    }
}

fn environment_value(name: &str) -> Result<Option<String>, crate::DaemonError> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or(crate::DaemonError::InvalidConfiguration)
        })
        .transpose()
}

fn validate_subject(subject: &str) -> Result<(), crate::DaemonError> {
    if subject.trim().is_empty() || subject.len() > 256 || subject.chars().any(char::is_control) {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_remote_grants(grants: &[Capability]) -> Result<(), crate::DaemonError> {
    if grants.is_empty()
        || grants.len() > CapabilityCatalog::all().len()
        || grants.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    Ok(())
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), crate::DaemonError> {
    use std::io::Write;
    #[cfg(windows)]
    let mut file = match crate::windows_security::create_current_user_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(crate::DaemonError::InvalidConfiguration);
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
        match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
            Err(_) => return Err(crate::DaemonError::InvalidConfiguration),
        }
    };
    file.write_all(bytes)
        .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    file.sync_all()
        .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    Ok(())
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

#[derive(Clone, Debug)]
pub(crate) struct RemoteClientConfig {
    pub principal_id: PrincipalId,
    pub pairing_verifier: VerifyingKey,
    pub bootstrap_grants: Vec<Capability>,
    pub subject: String,
}

/// Redacted result of trusted local remote-principal provisioning.
///
/// The enrollment file contains a private key and is deliberately returned only
/// as a path; no API serializes or prints its contents.
#[derive(Clone, Debug)]
pub struct RemoteEnrollment {
    pub principal_id: PrincipalId,
    pub enrollment_path: PathBuf,
    pub subject: String,
    pub grants: Vec<Capability>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PrivateEnrollment {
    endpoint_name: String,
    principal_id: uuid::Uuid,
    signing_key: [u8; 32],
}

#[derive(serde::Deserialize, serde::Serialize)]
struct RemoteClientManifest {
    clients: Vec<RemoteClientEntry>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct RemoteClientEntry {
    principal_id: uuid::Uuid,
    pairing_verifier: [u8; 32],
    bootstrap_grants: Vec<String>,
    #[serde(default)]
    subject: String,
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

pub(crate) fn load_explicit_enrollment(path: &Path) -> Result<IpcEnrollment, crate::DaemonError> {
    let enrollment: PrivateEnrollment = serde_json::from_slice(
        &fs::read(path).map_err(|_| crate::DaemonError::InvalidConfiguration)?,
    )
    .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    if enrollment.endpoint_name.trim().is_empty() || enrollment.endpoint_name.len() > 253 {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    Ok(IpcEnrollment {
        endpoint_name: enrollment.endpoint_name,
        principal_id: PrincipalId::try_from(enrollment.principal_id)
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
        signer: SigningKey::from_bytes(&enrollment.signing_key),
    })
}

fn load_remote_clients(
    database_path: &Path,
) -> Result<Vec<RemoteClientConfig>, crate::DaemonError> {
    let path = remote_manifest_path(database_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let manifest: RemoteClientManifest = serde_json::from_slice(
        &fs::read(path).map_err(|_| crate::DaemonError::InvalidConfiguration)?,
    )
    .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    if manifest.clients.len() > MAX_REMOTE_CLIENTS {
        return Err(crate::DaemonError::InvalidConfiguration);
    }
    let mut clients = Vec::with_capacity(manifest.clients.len());
    for entry in manifest.clients {
        let principal_id = PrincipalId::try_from(entry.principal_id)
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        if clients
            .iter()
            .any(|client: &RemoteClientConfig| client.principal_id == principal_id)
        {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let pairing_verifier = VerifyingKey::from_bytes(&entry.pairing_verifier)
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        let bootstrap_grants = entry
            .bootstrap_grants
            .into_iter()
            .map(|name| {
                Capability::from_mcp_name(&name).ok_or(crate::DaemonError::InvalidConfiguration)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if bootstrap_grants.is_empty() {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        clients.push(RemoteClientConfig {
            principal_id,
            pairing_verifier,
            bootstrap_grants,
            subject: entry.subject,
        });
    }
    Ok(clients)
}

fn write_remote_clients(
    database_path: &Path,
    clients: &[RemoteClientConfig],
) -> Result<(), crate::DaemonError> {
    let manifest = RemoteClientManifest {
        clients: clients
            .iter()
            .map(|client| RemoteClientEntry {
                principal_id: client.principal_id.into(),
                pairing_verifier: client.pairing_verifier.to_bytes(),
                bootstrap_grants: client
                    .bootstrap_grants
                    .iter()
                    .map(|capability| capability.metadata().mcp_name.to_owned())
                    .collect(),
                subject: client.subject.clone(),
            })
            .collect(),
    };
    let bytes =
        serde_json::to_vec(&manifest).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
    fs::write(remote_manifest_path(database_path), bytes)
        .map_err(|_| crate::DaemonError::InvalidConfiguration)
}

fn remote_manifest_path(database_path: &Path) -> PathBuf {
    database_path.with_extension("cortexd-clients.json")
}

fn remote_enrollment_path(database_path: &Path, principal_id: PrincipalId) -> PathBuf {
    database_path.with_extension(format!(
        "cortexd-client-{}.json",
        uuid::Uuid::from(principal_id)
    ))
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
