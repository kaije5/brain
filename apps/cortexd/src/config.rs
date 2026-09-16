use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use cortex_application::{Capability, CapabilityCatalog, SecretRef};
use cortex_domain::{PrincipalId, WorkspaceId};
use cortex_inference::{OpenAiCompatibleConfig, ProviderLimits};
use cortex_storage::RemoteEnrollmentRecord;

use crate::vault::VaultProviderConfig;
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

pub(crate) const MAX_REMOTE_CLIENTS: usize = 16;
const MODEL_RESPONSE_BYTES: usize = 64 * 1024;
const MODEL_EMBEDDING_INPUT_BYTES: usize = 32 * 1024;
const MODEL_EMBEDDING_DIMENSIONS: usize = 4096;

/// Raw inference credential held only by the daemon process. `Debug` is
/// redacted so the value can never reach logs or diagnostics.
#[derive(Clone, Default)]
pub struct InferenceBearer(String);

impl InferenceBearer {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for InferenceBearer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InferenceBearer([redacted])")
    }
}

/// Configuration owned locally by the daemon process, never by an IPC caller.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) endpoint_name: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) principal_id: PrincipalId,
    pub(crate) inference_secret: Option<SecretRef>,
    pub(crate) inference_bearer: Option<InferenceBearer>,
    pub(crate) model_config: Option<OpenAiCompatibleConfig>,
    pub(crate) pairing_verifier: VerifyingKey,
    pub(crate) pairing_signer: SigningKey,
    pub(crate) pairing_key_path: PathBuf,
    pub(crate) discovery_path: PathBuf,
    pub(crate) bootstrap_grants: Vec<Capability>,
    pub(crate) remote_clients: Vec<RemoteClientConfig>,
    pub(crate) vault: Option<VaultProviderConfig>,
    pub(crate) prompt: PromptConfig,
}

/// Operator-configured Brain prompt layers (SCRUM-147): a persistent global
/// instruction (inline value or file reference, resolved at composition)
/// plus optional per-profile instructions. Cortex's protected instructions
/// are not configurable and live at the composition site.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PromptConfig {
    /// Inline global Brain instructions; mutually exclusive with
    /// `global_file`.
    pub global_inline: Option<String>,
    /// File reference for global Brain instructions, read once at
    /// composition.
    pub global_file: Option<PathBuf>,
    /// Optional per-profile Brain instructions keyed by profile id.
    pub profiles: BTreeMap<String, String>,
}

impl PromptConfig {
    /// Resolves the configured prompt layers into concrete values, reading a
    /// declared prompt file exactly once. Composition afterwards is pure, so
    /// an unchanged configuration renders a byte-identical Stable prefix and
    /// file edits take effect only at the next daemon start.
    ///
    /// # Errors
    /// Returns a redacted configuration error when a declared prompt file is
    /// missing, unreadable, or invalid.
    pub fn resolve(self) -> Result<ResolvedPromptConfig, crate::DaemonError> {
        let global = match (self.global_inline, self.global_file) {
            (Some(inline), None) => Some(inline),
            (None, Some(file)) => Some(
                std::fs::read_to_string(&file)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
            ),
            (None, None) => None,
            (Some(_), Some(_)) => return Err(crate::DaemonError::InvalidConfiguration),
        };
        Ok(ResolvedPromptConfig {
            global,
            profiles: self.profiles,
        })
    }
}

/// Resolved, immutable prompt configuration for one daemon process lifetime.
/// Configuration changes apply at the next daemon start (SCRUM-147 refresh
/// semantics) and never mutate in-flight requests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedPromptConfig {
    pub(crate) global: Option<String>,
    pub(crate) profiles: BTreeMap<String, String>,
}

impl DaemonConfig {
    /// Creates deterministic test-only local configuration with fresh ownership IDs.
    #[must_use]
    pub fn for_test(directory: &Path) -> Self {
        let mut config = Self::with_fresh_pairing(
            directory.join("cortex.db"),
            format!("cortexd-test-{}", uuid::Uuid::now_v7()),
            WorkspaceId::new(),
            PrincipalId::new(),
            directory.join("cortexd-discovery.json"),
        );
        config.vault = default_vault_config(&config.database_path);
        config
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
            inference_bearer: None,
            model_config: None,
            pairing_verifier: signer.verifying_key(),
            pairing_signer: signer,
            pairing_key_path: pairing_key_path(&discovery_path),
            discovery_path,
            bootstrap_grants: CapabilityCatalog::all().to_vec(),
            remote_clients: Vec::new(),
            vault: None,
            prompt: PromptConfig::default(),
        }
    }

    /// Loads a daemon configuration without accepting any identity from a client.
    ///
    /// # Errors
    /// Returns a safe configuration error when the database path is absent.
    pub fn from_database_path(database_path: PathBuf) -> Result<Self, crate::DaemonError> {
        Self::from_local_settings(database_path, None)
    }

    /// Loads a daemon configuration from the local `cortexd.toml` settings
    /// (when present) with documented defaults when the file is absent.
    ///
    /// # Errors
    /// Returns a safe configuration error when the database path is absent or
    /// durable local artifacts cannot be reconciled.
    pub fn from_local_settings(
        database_path: PathBuf,
        settings: Option<&crate::settings::LocalSettings>,
    ) -> Result<Self, crate::DaemonError> {
        let prompt = settings
            .map(crate::settings::LocalSettings::prompt_config)
            .transpose()
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?
            .unwrap_or_default();
        let vault = match settings.map(crate::settings::LocalSettings::vault_config) {
            None => None,
            Some(Ok(vault)) => vault,
            // Missing, invalid, or contradictory vault declarations fail
            // startup with a typed diagnostic instead of degrading silently.
            Some(Err(_)) => return Err(crate::DaemonError::InvalidConfiguration),
        };
        if let Some(vault) = &vault {
            vault
                .validate_root_access()
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        }
        Self::from_database_path_with_endpoint(
            database_path,
            settings.and_then(crate::settings::LocalSettings::endpoint_override),
            vault,
            prompt,
        )
    }

    fn from_database_path_with_endpoint(
        database_path: PathBuf,
        endpoint_override: Option<&str>,
        vault: Option<VaultProviderConfig>,
        prompt: PromptConfig,
    ) -> Result<Self, crate::DaemonError> {
        if database_path.as_os_str().is_empty() {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        if let Some(parent) = database_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        }
        let discovery_path = database_path.with_extension("cortexd-discovery.json");
        let pairing_key_path = pairing_key_path(&discovery_path);
        let default_root = database_path.clone();
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
                inference_bearer: None,
                model_config: None,
                pairing_verifier,
                pairing_signer,
                pairing_key_path,
                discovery_path,
                bootstrap_grants: CapabilityCatalog::all().to_vec(),
                remote_clients,
                vault: vault.or_else(|| default_vault_config(default_root.as_path())),
                prompt,
            });
        }
        let mut config = Self::with_fresh_pairing(
            database_path,
            endpoint_override.map_or_else(
                || format!("cortexd-{}", uuid::Uuid::now_v7()),
                str::to_owned,
            ),
            WorkspaceId::new(),
            PrincipalId::new(),
            discovery_path,
        );
        config.vault = vault.or_else(|| default_vault_config(&config.database_path));
        config.prompt = prompt;
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

    /// Attaches the resolved inference credential for authenticated providers.
    /// Held only for the process lifetime; `Debug` stays redacted.
    #[must_use]
    pub fn with_inference_bearer(mut self, bearer: Option<InferenceBearer>) -> Self {
        self.inference_bearer = bearer;
        self
    }

    /// Narrows bootstrap grants for an isolated daemon instance. Production configuration keeps
    /// the explicit complete owner grant set unless an owner administration flow changes it.
    #[must_use]
    pub fn with_bootstrap_grants(mut self, grants: Vec<Capability>) -> Self {
        self.bootstrap_grants = grants;
        self
    }

    /// Attaches a validated vault provider configuration. Composition roots
    /// that cannot read the declared root must not mount it: providers are
    /// injected only with an accessible, validated configuration.
    ///
    /// # Errors
    /// Returns a redacted configuration error when the declared root is
    /// missing or not a directory.
    pub fn with_vault_provider(
        mut self,
        config: VaultProviderConfig,
    ) -> Result<Self, crate::DaemonError> {
        config
            .validate_root_access()
            .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        self.vault = Some(config);
        Ok(self)
    }

    /// Attaches the operator's prompt configuration (SCRUM-147).
    #[must_use]
    pub fn with_prompt_config(mut self, prompt: PromptConfig) -> Self {
        self.prompt = prompt;
        self
    }

    /// Returns the configured vault provider configuration, when present.
    #[must_use]
    pub const fn vault_provider(&self) -> Option<&VaultProviderConfig> {
        self.vault.as_ref()
    }

    /// Returns the local IPC endpoint name for diagnostics and tests.
    #[must_use]
    pub fn endpoint_name(&self) -> &str {
        &self.endpoint_name
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

    /// Reconciles the protected local artifact and verifier manifest for an enrollment that has
    /// already committed through the daemon's `SQLite` operation/audit transaction. This method
    /// never creates a new identity: the durable record fixes the subject, principal, grants, and
    /// verifier, so retries after an interrupted filesystem write converge on the same artifact.
    ///
    /// # Errors
    /// Returns a redacted configuration error when the durable record, protected artifact, or
    /// local verifier manifest cannot be reconciled safely.
    pub fn reconcile_remote_enrollment(
        &mut self,
        record: &RemoteEnrollmentRecord,
    ) -> Result<RemoteEnrollment, crate::DaemonError> {
        validate_subject(&record.subject)?;
        validate_remote_grants(&record.grants)?;
        if record.principal_id == self.principal_id {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let signer = self.derived_remote_signing_key(record.principal_id);
        if signer.verifying_key().to_bytes() != record.pairing_verifier {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        let enrollment_path = remote_enrollment_path(&self.database_path, record.principal_id);
        if enrollment_path.exists() {
            let existing = load_explicit_enrollment(&enrollment_path)?;
            if existing.principal_id != record.principal_id
                || existing.endpoint_name != self.endpoint_name
                || existing.signer.verifying_key() != signer.verifying_key()
            {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
        } else {
            let enrollment = PrivateEnrollment {
                endpoint_name: self.endpoint_name.clone(),
                principal_id: record.principal_id.into(),
                signing_key: signer.to_bytes(),
            };
            write_private_bytes(
                &enrollment_path,
                &serde_json::to_vec(&enrollment)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
            )?;
        }
        let client = RemoteClientConfig {
            principal_id: record.principal_id,
            pairing_verifier: signer.verifying_key(),
            bootstrap_grants: record.grants.clone(),
            subject: record.subject.clone(),
        };
        if let Some(existing) = self
            .remote_clients
            .iter()
            .find(|existing| existing.subject == record.subject)
        {
            if existing.principal_id != client.principal_id
                || existing.pairing_verifier != client.pairing_verifier
                || existing.bootstrap_grants != client.bootstrap_grants
            {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
        } else {
            if self.remote_clients.len() >= MAX_REMOTE_CLIENTS
                || self
                    .remote_clients
                    .iter()
                    .any(|existing| existing.principal_id == client.principal_id)
            {
                return Err(crate::DaemonError::InvalidConfiguration);
            }
            self.remote_clients.push(client);
            write_remote_clients(&self.database_path, &self.remote_clients)?;
        }
        Ok(RemoteEnrollment {
            principal_id: record.principal_id,
            enrollment_path,
            subject: record.subject.clone(),
            grants: record.grants.clone(),
        })
    }

    /// Derives a per-principal artifact key from the protected owner pairing key. The derivation
    /// happens in memory and is domain-separated, allowing post-commit reconciliation without
    /// persisting a remote private key before the database/audit transaction is durable.
    #[must_use]
    pub fn derived_remote_signing_key(&self, principal_id: PrincipalId) -> SigningKey {
        let mut digest = Sha256::new();
        digest.update(b"cortex-v0.1 remote enrollment key\0");
        digest.update(self.pairing_signer.to_bytes());
        digest.update(uuid::Uuid::from(principal_id).as_bytes());
        SigningKey::from_bytes(&digest.finalize().into())
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

/// The default local vault root beside the daemon database, used when no
/// settings configure one. The initial desktop deployment may use any safe
/// local vault path (storage plan §4).
fn default_vault_config(database_path: &Path) -> Option<VaultProviderConfig> {
    let root = database_path.parent()?.join("vault");
    fs::create_dir_all(&root).ok()?;
    let mut scopes = std::collections::BTreeSet::new();
    scopes.insert(crate::vault::VaultScope::new("knowledge").ok()?);
    scopes.insert(crate::vault::VaultScope::new("task").ok()?);
    VaultProviderConfig::new(
        "markdown-vault",
        root,
        crate::vault::VaultProviderMode::ReadWrite,
        scopes,
        std::collections::BTreeSet::new(),
    )
    .ok()
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
