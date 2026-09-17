use std::{collections::BTreeSet, io, path::PathBuf, sync::Arc, time::Duration};

use cortex_application::{
    ApplicationError, ApplicationService, AuditPort, Capability, CapabilityCatalog,
    CapabilityGrant, CommandContext, GrantPolicy, KnowledgeCreate, KnowledgeDelete,
    KnowledgeUpdate, MemoryCorrectInput, MemoryCreateInput, ProviderAuthority,
    ProviderMutationOutcome, ProviderTask, SecretStore, TaskComplete, TaskCreate, TaskDelete,
    TaskProvider, TaskQuery, TaskSchedulingMetadata, TaskUpdate,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, ObservedRevision, OperationId, PolicyDecision,
    PolicyDeny, PrincipalId, ProviderResourceKind, ProviderResourceRef, Revision, SourceRef,
    TaskId, WorkspaceId,
};
use cortex_inference::{
    AgentLimits, AgentRunner, AuthorizedCapabilities, OpenAiCompatibleProvider, PromptLayers,
    ReqwestOpenAiTransport,
};
use cortex_search::{
    DerivedVaultIndex, FusedLeg, HybridSearchService, RetrievalOutcome, SearchHit, SearchRequest,
    fuse_with_memories, hybrid as vault_hybrid,
};
use cortex_storage::{
    OperationStore, ProviderOperationStore, RemoteEnrollmentRequest, SqliteAuditPort,
    SqliteDatabase, SqliteRepositories,
};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use uuid::{Uuid, Version};

use crate::{
    DaemonConfig, MarkdownVaultProvider, config::MAX_REMOTE_CLIENTS, config::ResolvedPromptConfig,
    vault::VaultProviderConfig,
};

/// The only supported local IPC protocol version. v2 carries provider
/// resource identity and opaque observed revisions for user content; no
/// earlier version is accepted (SCRUM-117).
pub const PROTOCOL_VERSION: u16 = 2;

/// Cortex's protected Stable-tier instructions: mandatory security, policy
/// and runtime guidance that operator configuration can extend but never
/// remove or replace (SCRUM-147).
pub const PROTECTED_CORTEX_PROMPT: &str = "You are Cortex, a local-first personal knowledge agent. \
You can read and change the user's notes, tasks, and memories \
through the provided tools. Prefer a tool over guessing, never \
fabricate entity identifiers, and keep answers concise.";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_SEARCH_RESULTS: usize = 32;
const MAX_SEARCH_SNIPPET_BYTES: usize = 1024;

/// A client envelope. The claimed principal is deliberately ignored after OS-local authentication.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DaemonRequest {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub principal_id: Uuid,
    pub operation_id: Uuid,
    pub capability: String,
    pub payload: Value,
}

/// One daemon-generated, single-use challenge for proving possession of a paired private key.
/// The discovery record contains only the corresponding public verifier.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PairingChallenge {
    pub protocol_version: u16,
    pub nonce: Uuid,
}

/// Client proof over the exact challenge. Signatures are fixed-size Ed25519 values.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PairingResponse {
    pub protocol_version: u16,
    pub signature: Vec<u8>,
}

/// A bounded versioned daemon response correlated to its request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DaemonResponse {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub result: WireResult,
}

/// Bounded, redacted daemon result carried on the wire for both success and failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireResult {
    Success { value: Value },
    Error { code: String },
}

/// Safe local IPC failure categories. They intentionally omit filesystem and database details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonError {
    Unauthenticated,
    InvalidRequest,
    UnsupportedCapability,
    PermissionDenied,
    Conflict,
    NotFound,
    InvalidConfiguration,
    StartupFailed,
    TransportUnavailable,
    SecretStoreUnavailable,
    RateLimited,
    AuthenticationFailed,
    QuotaExceeded,
    ContextOverflow,
    InvalidInferenceRequest,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unauthenticated => "local client is not authenticated",
            Self::Conflict => "the resource changed since the caller last observed it",
            Self::NotFound => "the addressed resource does not exist",
            Self::InvalidRequest => "invalid local IPC request",
            Self::UnsupportedCapability => "unsupported daemon capability",
            Self::PermissionDenied => "daemon capability is not granted",
            Self::InvalidConfiguration => "invalid daemon configuration",
            Self::StartupFailed => "daemon startup failed",
            Self::TransportUnavailable => "local transport unavailable",
            Self::SecretStoreUnavailable => "OS keyring is unavailable or locked",
            Self::RateLimited => "provider rate limited the request",
            Self::AuthenticationFailed => "provider rejected the credential",
            Self::QuotaExceeded => "provider quota or billing limit reached",
            Self::ContextOverflow => "request exceeded the model context window",
            Self::InvalidInferenceRequest => "provider rejected the request",
        })
    }
}

impl std::error::Error for DaemonError {}

/// Secret-free vault health facts for doctor/status diagnostics (SCRUM-133).
/// Index counters come from the last full reconciliation; `fresh` is false
/// when the last refresh failed or the vault root became inaccessible.
#[derive(Clone, Debug, Default)]
struct VaultHealth {
    refreshed_at: Option<String>,
    indexed: usize,
    unchanged: usize,
    removed: usize,
    skipped: usize,
    fresh: bool,
    /// Observed at health-write time; never carries content or secrets.
    root_accessible: bool,
}

impl VaultHealth {
    fn from_report(
        report: &crate::vault_watcher::ReconciliationReport,
        root_accessible: bool,
    ) -> Self {
        Self {
            refreshed_at: Some(chrono::Utc::now().to_rfc3339()),
            indexed: report.indexed,
            unchanged: report.unchanged,
            removed: report.removed,
            skipped: report.skipped,
            fresh: true,
            root_accessible,
        }
    }
}

/// State-owning Cortex daemon. `SQLite` is opened and migrated before this value is returned.
#[derive(Clone)]
pub struct LocalDaemon {
    database: SqliteDatabase,
    database_path: PathBuf,
    endpoint_name: String,
    workspace_id: WorkspaceId,
    owner_principal_id: PrincipalId,
    owner_pairing_signer: ed25519_dalek::SigningKey,
    client_verifiers: Vec<ClientVerifier>,
    migrations_applied: bool,
    service: Arc<DaemonService>,
    search: Arc<DaemonSearch>,
    embedding_provider: SharedEmbeddingProvider,
    discovered_models: Arc<std::sync::RwLock<Vec<String>>>,
    grants: BTreeSet<CapabilityGrant>,
    audit: SqliteAuditPort,
    authority: Option<Arc<DaemonAuthority>>,
    vault: Option<Arc<MarkdownVaultProvider>>,
    /// Secret-free vault health snapshot for doctor/status diagnostics.
    vault_health: Arc<std::sync::RwLock<VaultHealth>>,
    /// The resolved vault configuration (root/scope facts only).
    vault_config: Option<VaultProviderConfig>,
    /// Derived, rebuildable vault index backing fused knowledge retrieval.
    vault_index: Arc<std::sync::RwLock<DerivedVaultIndex>>,
    /// Resolved Brain prompt configuration: immutable for the process
    /// lifetime, so configuration changes apply only at the next daemon
    /// start and never mutate in-flight requests (SCRUM-147).
    prompt: ResolvedPromptConfig,
    resolved_profile: Arc<std::sync::RwLock<Option<String>>>,
}

#[derive(Clone)]
struct ClientVerifier {
    principal_id: PrincipalId,
    pairing_verifier: VerifyingKey,
}

type DaemonService =
    ApplicationService<GrantPolicy, SqliteRepositories, OperationStore, SqliteAuditPort>;

/// A mutation result from either the provider authority (user content) or
/// the legacy aggregate service (Cortex-owned runtime state like memories).
enum MutationOutcome {
    Provider(ProviderMutationOutcome),
    Legacy(cortex_application::MutationResult),
}

/// Provider-backed authority for user-authored knowledge and tasks: the only
/// surface that reads or mutates canonical user content (SCRUM-116). The
/// vault provider implements both provider ports; `SQLite` retains runtime
/// state (operation identity log, audit) but no note/task authority.
type DaemonAuthority = ProviderAuthority<
    GrantPolicy,
    MarkdownVaultProvider,
    MarkdownVaultProvider,
    ProviderOperationStore,
    SqliteAuditPort,
>;
type DaemonSearch = HybridSearchService<SqliteRepositories, SharedEmbeddingProvider>;

#[derive(Clone, Default)]
enum DaemonEmbeddingProvider {
    #[default]
    Unavailable,
    Configured(Arc<OpenAiCompatibleProvider<ReqwestOpenAiTransport>>),
}

impl cortex_application::EmbeddingProvider for DaemonEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<cortex_application::Embedding, ApplicationError> {
        match self {
            Self::Unavailable => Err(ApplicationError::InferenceUnavailable),
            Self::Configured(provider) => {
                cortex_application::EmbeddingProvider::embed(provider.as_ref(), text).await
            }
        }
    }
}

impl cortex_inference::InferenceProvider for DaemonEmbeddingProvider {
    async fn complete(
        &self,
        request: cortex_inference::InferenceRequest,
    ) -> Result<cortex_inference::InferenceResponse, ApplicationError> {
        match self {
            Self::Unavailable => Err(ApplicationError::InferenceUnavailable),
            Self::Configured(provider) => {
                cortex_inference::InferenceProvider::complete(provider.as_ref(), request).await
            }
        }
    }

    async fn complete_streaming(
        &self,
        request: cortex_inference::InferenceRequest,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<cortex_inference::InferenceResponse, ApplicationError> {
        match self {
            Self::Unavailable => Err(ApplicationError::InferenceUnavailable),
            Self::Configured(provider) => {
                cortex_inference::InferenceProvider::complete_streaming(
                    provider.as_ref(),
                    request,
                    on_delta,
                )
                .await
            }
        }
    }
}

/// Model provider shared between the search service and the agent loop.
/// The daemon starts with [`DaemonEmbeddingProvider::Unavailable`] and
/// installs the resolved provider once background model resolution
/// completes, so IPC availability never waits on catalog probing.
#[derive(Clone, Default)]
struct SharedEmbeddingProvider(Arc<std::sync::RwLock<DaemonEmbeddingProvider>>);

impl SharedEmbeddingProvider {
    fn install(&self, provider: DaemonEmbeddingProvider) {
        *self.0.write().expect("provider lock") = provider;
    }

    fn current(&self) -> DaemonEmbeddingProvider {
        self.0.read().expect("provider lock").clone()
    }
}

impl cortex_application::EmbeddingProvider for SharedEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<cortex_application::Embedding, ApplicationError> {
        cortex_application::EmbeddingProvider::embed(&self.current(), text).await
    }
}

impl cortex_inference::InferenceProvider for SharedEmbeddingProvider {
    async fn complete_streaming(
        &self,
        request: cortex_inference::InferenceRequest,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<cortex_inference::InferenceResponse, ApplicationError> {
        cortex_inference::InferenceProvider::complete_streaming(&self.current(), request, on_delta)
            .await
    }

    async fn complete(
        &self,
        request: cortex_inference::InferenceRequest,
    ) -> Result<cortex_inference::InferenceResponse, ApplicationError> {
        cortex_inference::InferenceProvider::complete(&self.current(), request).await
    }
}

struct DaemonAgentExecutor {
    daemon: LocalDaemon,
}

impl cortex_application::AgentCapabilityExecutor for DaemonAgentExecutor {
    async fn execute_agent_tool(
        &self,
        context: CommandContext,
        capability: Capability,
        payload: Value,
    ) -> Result<Value, ApplicationError> {
        let response = Box::pin(self.daemon.request_authenticated(
            context.principal_id,
            &DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: context.correlation_id,
                principal_id: Uuid::from(context.principal_id),
                operation_id: Uuid::from(context.operation_id),
                capability: capability.metadata().mcp_name.to_owned(),
                payload,
            },
        ))
        .await
        .map_err(|error| application_error_from_daemon(&error))?;
        match response.result {
            WireResult::Success { value } => Ok(value),
            WireResult::Error { code } if code == "permission_denied" => {
                Err(ApplicationError::PermissionDenied)
            }
            WireResult::Error { .. } => Err(ApplicationError::Internal),
        }
    }
}

fn application_error_from_daemon(error: &DaemonError) -> ApplicationError {
    match error {
        DaemonError::PermissionDenied => ApplicationError::PermissionDenied,
        DaemonError::Conflict => ApplicationError::Conflict { entity: "resource" },
        DaemonError::NotFound => ApplicationError::NotFound { entity: "resource" },
        DaemonError::InvalidRequest | DaemonError::InvalidInferenceRequest => {
            ApplicationError::Validation { field: "payload" }
        }
        DaemonError::RateLimited => ApplicationError::RateLimited {
            retry_after_secs: None,
        },
        DaemonError::AuthenticationFailed => ApplicationError::AuthenticationFailed,
        DaemonError::QuotaExceeded => ApplicationError::QuotaExceeded,
        DaemonError::ContextOverflow => ApplicationError::ContextOverflow,
        DaemonError::SecretStoreUnavailable => ApplicationError::SecretStoreUnavailable,
        DaemonError::Unauthenticated
        | DaemonError::UnsupportedCapability
        | DaemonError::InvalidConfiguration
        | DaemonError::StartupFailed
        | DaemonError::TransportUnavailable => ApplicationError::Internal,
    }
}

/// A daemon-issued in-process handle representing a completed local authentication handshake.
/// Its constructor is private so an IPC payload can never manufacture a trusted principal.
#[derive(Clone)]
pub struct AuthenticatedLocalClient {
    daemon: LocalDaemon,
    principal_id: PrincipalId,
}

/// Private-key material provisioned separately from public discovery for an intended local IPC
/// client. It can answer a fresh daemon challenge but cannot create a trusted daemon principal.
#[derive(Clone)]
pub struct ProvisionedLocalClient {
    pairing_signer: ed25519_dalek::SigningKey,
}

impl ProvisionedLocalClient {
    #[must_use]
    pub(crate) const fn new(pairing_signer: ed25519_dalek::SigningKey) -> Self {
        Self { pairing_signer }
    }

    #[must_use]
    pub fn pairing_response(&self, challenge: &PairingChallenge) -> PairingResponse {
        pairing_response_for(&self.pairing_signer, challenge)
    }
}

impl AuthenticatedLocalClient {
    /// Sends one request after the daemon-owned pairing/OS-local authentication boundary.
    ///
    /// # Errors
    /// Returns a safe protocol error for an invalid or unsupported request.
    pub async fn request(&self, request: &DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        Ok(self
            .daemon
            .handle_request_for(self.principal_id, request.clone())
            .await)
    }

    #[must_use]
    pub fn pairing_response(&self, challenge: &PairingChallenge) -> PairingResponse {
        self.daemon.pairing_response(challenge)
    }
}

impl LocalDaemon {
    /// Opens and migrates `SQLite` before making any IPC endpoint available.
    ///
    /// # Errors
    /// Returns only a redacted startup failure category.
    pub async fn start(config: DaemonConfig) -> Result<Self, DaemonError> {
        if config.inference_secret.is_some()
            || config
                .model_config
                .as_ref()
                .and_then(cortex_inference::OpenAiCompatibleConfig::secret_reference)
                .is_some()
        {
            return Err(DaemonError::InvalidConfiguration);
        }
        Self::start_inner(config).await
    }

    /// Resolves opaque configuration secret references once, at the process composition root.
    /// The resolved reference is not logged, serialized, stored, or passed to IPC.
    ///
    /// # Errors
    /// Returns a redacted startup error if the platform secret store or local database fails.
    pub async fn start_with_secret_store<S>(
        config: DaemonConfig,
        secret_store: &S,
    ) -> Result<Self, DaemonError>
    where
        S: SecretStore,
    {
        if let Some(reference) = config.inference_secret.as_ref() {
            let _resolved = secret_store
                .resolve(reference)
                .await
                .map_err(|_| DaemonError::StartupFailed)?;
        }
        if let Some(reference) = config
            .model_config
            .as_ref()
            .and_then(cortex_inference::OpenAiCompatibleConfig::secret_reference)
        {
            let _resolved = secret_store
                .resolve(reference)
                .await
                .map_err(|_| DaemonError::StartupFailed)?;
        }
        Self::start_inner(config).await
    }

    #[allow(clippy::too_many_lines)] // One ordered startup boundary keeps composition reviewable.
    async fn start_inner(config: DaemonConfig) -> Result<Self, DaemonError> {
        config.ensure_pairing_key()?;
        let database_path = config.database_path.clone();
        let database = SqliteDatabase::connect_and_migrate(&database_path)
            .await
            .map_err(|_| DaemonError::StartupFailed)?;
        let repositories = database.repositories();
        repositories
            .bootstrap_owner(
                config.workspace_id,
                config.principal_id,
                &config.bootstrap_grants,
            )
            .await
            .map_err(|_| DaemonError::StartupFailed)?;
        let committed_remote_enrollments = database
            .operation_store()
            .remote_enrollments(config.workspace_id)
            .await
            .map_err(|_| DaemonError::StartupFailed)?;
        for remote in &config.remote_clients {
            let committed = committed_remote_enrollments
                .iter()
                .find(|record| record.subject == remote.subject)
                .ok_or(DaemonError::StartupFailed)?;
            if committed.principal_id != remote.principal_id
                || committed.pairing_verifier != remote.pairing_verifier.to_bytes()
                || committed.grants != remote.bootstrap_grants
            {
                return Err(DaemonError::StartupFailed);
            }
        }
        let mut client_verifiers = vec![ClientVerifier {
            principal_id: config.principal_id,
            pairing_verifier: config.pairing_verifier,
        }];
        client_verifiers.extend(config.remote_clients.iter().map(|remote| ClientVerifier {
            principal_id: remote.principal_id,
            pairing_verifier: remote.pairing_verifier,
        }));
        let vault_config = config.vault_provider().cloned();
        let mut grants = BTreeSet::new();
        for client in &client_verifiers {
            let persisted_capabilities = repositories
                .granted_capabilities(config.workspace_id, client.principal_id)
                .await
                .map_err(|_| DaemonError::StartupFailed)?;
            grants.extend(persisted_capabilities.into_iter().map(|capability| {
                CapabilityGrant::new(config.workspace_id, client.principal_id, capability)
            }));
        }
        let embedding_provider = SharedEmbeddingProvider::default();
        if let Some(provider) = config.model_config {
            let bearer = config
                .inference_bearer
                .as_ref()
                .map(|credential| credential.expose().to_owned());
            embedding_provider.install(DaemonEmbeddingProvider::Configured(Arc::new(
                OpenAiCompatibleProvider::new(provider).with_bearer(bearer),
            )));
        }
        let service = Arc::new(ApplicationService::new(
            GrantPolicy::new(grants.iter().copied()),
            repositories.clone(),
            database.operation_store(),
            database.audit_port(),
        ));
        let audit = database.audit_port();
        let prompt = config
            .prompt
            .clone()
            .resolve()
            .map_err(|_| DaemonError::InvalidConfiguration)?;
        let (authority, vault, vault_index, vault_health) = match vault_config.as_ref() {
            Some(vault_config) => {
                let opened = MarkdownVaultProvider::open(vault_config.clone(), config.workspace_id)
                    .map_err(|_| DaemonError::StartupFailed)?;
                let vault = Arc::new(opened.clone());
                let authority = Arc::new(ProviderAuthority::new(
                    GrantPolicy::new(grants.iter().copied()),
                    opened.clone(),
                    opened,
                    database.provider_operation_store(),
                    database.audit_port(),
                ));
                let mut index = DerivedVaultIndex::new();
                let report = crate::vault_watcher::rebuild_vault(&vault, &mut index)
                    .map_err(|_| DaemonError::StartupFailed)?;
                let mut health =
                    VaultHealth::from_report(&report, vault_config.validate_root_access().is_ok());
                health.fresh = true;
                (Some(authority), Some(vault), index, health)
            }
            None => (None, None, DerivedVaultIndex::new(), VaultHealth::default()),
        };
        let vault_index = Arc::new(std::sync::RwLock::new(vault_index));
        let vault_health = Arc::new(std::sync::RwLock::new(vault_health));
        Ok(Self {
            database,
            database_path,
            endpoint_name: config.endpoint_name,
            workspace_id: config.workspace_id,
            owner_principal_id: config.principal_id,
            owner_pairing_signer: config.pairing_signer,
            client_verifiers,
            migrations_applied: true,
            service,
            search: Arc::new(HybridSearchService::new(
                repositories,
                embedding_provider.clone(),
            )),
            embedding_provider: embedding_provider.clone(),
            discovered_models: Arc::new(std::sync::RwLock::new(Vec::new())),
            grants,
            audit,
            authority,
            vault,
            vault_health,
            vault_config: vault_config.clone(),
            vault_index,
            prompt,
            resolved_profile: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    /// Test-only access to the derived index for exercising rebuild flows.
    #[doc(hidden)]
    #[must_use]
    pub fn vault_index_for_test(&self) -> &Arc<std::sync::RwLock<DerivedVaultIndex>> {
        &self.vault_index
    }

    /// Rebuilds the derived vault index from current vault content. Called
    /// after vault mutations and by the watcher so fused retrieval stays
    /// current; fully rebuildable, so failure never corrupts state. On
    /// failure the previous index stays in place and health is marked stale.
    ///
    /// # Panics
    /// Panics if the derived index lock was poisoned by a prior panic.
    #[must_use]
    pub fn refresh_vault_index(&self) -> bool {
        let Some(vault) = &self.vault else {
            return false;
        };
        let root_accessible = self
            .vault_config
            .as_ref()
            .is_some_and(|config| config.validate_root_access().is_ok());
        let mut index = self
            .vault_index
            .write()
            .expect("vault index mutex poisoned");
        match crate::vault_watcher::rebuild_vault(vault, &mut index) {
            Ok(report) => {
                *self
                    .vault_health
                    .write()
                    .expect("vault health mutex poisoned") =
                    VaultHealth::from_report(&report, root_accessible);
                true
            }
            Err(_) => {
                let mut health = self
                    .vault_health
                    .write()
                    .expect("vault health mutex poisoned");
                health.fresh = false;
                health.refreshed_at = Some(chrono::Utc::now().to_rfc3339());
                health.root_accessible = root_accessible;
                false
            }
        }
    }

    /// Installs the resolved model provider and discovered catalog after
    /// startup, upgrading inference from its explicit unavailable state
    /// without blocking IPC availability (SCRUM-76).
    pub fn install_resolved_model(
        &self,
        config: cortex_inference::OpenAiCompatibleConfig,
        bearer: Option<String>,
        models: Vec<String>,
        profile_id: &str,
    ) {
        self.embedding_provider
            .install(DaemonEmbeddingProvider::Configured(Arc::new(
                OpenAiCompatibleProvider::new(config).with_bearer(bearer),
            )));
        if let Ok(mut catalog) = self.discovered_models.write() {
            *catalog = models;
        }
        if let Ok(mut resolved) = self.resolved_profile.write() {
            *resolved = Some(profile_id.to_owned());
        }
    }

    /// The composed Stable prompt tier for one resolved profile: Cortex's
    /// protected instructions, then the operator's global Brain prompt, then
    /// the profile instructions — deterministically ordered (SCRUM-147).
    #[must_use]
    pub fn composed_stable_prompt(&self, profile_id: Option<&str>) -> String {
        let compose = || -> Result<String, cortex_application::ApplicationError> {
            Ok(PromptLayers::new(PROTECTED_CORTEX_PROMPT)?
                .with_user_global(self.prompt.global.clone())?
                .with_profile(profile_id.and_then(|id| self.prompt.profiles.get(id).cloned()))?
                .compose_stable())
        };
        compose().unwrap_or_else(|_| PROTECTED_CORTEX_PROMPT.to_owned())
    }

    /// Persists the durable routing decision `{profile_id, model_id}` so
    /// diagnostics can report which profile served which model. Only
    /// identifiers and a timestamp are stored; secrets never reach storage
    /// (SCRUM-82).
    ///
    /// # Errors
    /// Returns the storage failure when the decision cannot be recorded.
    pub async fn record_route(
        &self,
        profile_id: &str,
        model_id: &str,
    ) -> Result<(), cortex_application::ApplicationError> {
        self.database
            .model_routing_store()
            .record_route(profile_id, model_id, &chrono::Utc::now().to_rfc3339())
            .await
    }

    /// The discovered model catalog when background resolution completed;
    /// empty while resolution is pending, disabled, or degraded.
    #[must_use]
    pub fn discovered_models(&self) -> Vec<String> {
        self.discovered_models
            .read()
            .map(|models| models.clone())
            .unwrap_or_default()
    }

    /// Reports whether ordered `SQLite` migrations completed before service availability.
    ///
    /// # Errors
    /// Returns a safe daemon error if startup state cannot be read.
    pub fn migrations_applied(&self) -> Result<bool, DaemonError> {
        let _ = &self.database;
        Ok(self.migrations_applied)
    }

    /// Returns the daemon-owned tenant identities for local diagnostics only.
    #[must_use]
    pub fn ownership_identity(&self) -> (Uuid, Uuid) {
        (self.workspace_id.into(), self.owner_principal_id.into())
    }

    /// Returns the daemon workspace for local administrative inspection.
    #[must_use]
    pub const fn ownership_workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Demonstrates the fail-closed unpaired-client path used by transport adapters.
    ///
    /// # Errors
    /// Always returns `Unauthenticated` because no local authentication was established.
    pub fn unpaired_status(&self) -> Result<DaemonResponse, DaemonError> {
        Err(DaemonError::Unauthenticated)
    }

    /// Returns a daemon-issued local client handle after trusted local authentication.
    #[must_use]
    pub fn paired_client(&self) -> AuthenticatedLocalClient {
        AuthenticatedLocalClient {
            daemon: self.clone(),
            principal_id: self.owner_principal_id,
        }
    }

    /// Decodes one bounded wire envelope before it is considered for authentication.
    ///
    /// # Errors
    /// Returns `InvalidRequest` for malformed, oversized, unsupported, or unversioned input.
    pub fn decode_request(&self, bytes: &[u8]) -> Result<DaemonRequest, DaemonError> {
        if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
            return Err(DaemonError::InvalidRequest);
        }
        let request: DaemonRequest =
            serde_json::from_slice(bytes).map_err(|_| DaemonError::InvalidRequest)?;
        validate_request(&request)?;
        Ok(request)
    }

    /// Handles a request after the platform-local transport has authenticated the peer.
    /// The request principal is not trusted: the daemon-owned local principal is authoritative.
    async fn request_authenticated(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        validate_request(request)?;
        let correlation_id = request.request_id;
        match request.capability.as_str() {
            "cortex_daemon_status" => {
                Ok(self.diagnostic_response(principal_id, correlation_id, "status"))
            }
            "cortex_daemon_doctor" => {
                Ok(self.diagnostic_response(principal_id, correlation_id, "doctor"))
            }
            "cortex_daemon_logs" => {
                Ok(self.diagnostic_response(principal_id, correlation_id, "logs"))
            }
            "cortex_model_list" => Ok(self.model_list_response(correlation_id)),
            "cortex_remote_enroll" => self.enroll_remote_principal(principal_id, request).await,
            "cortex_knowledge_search" | "cortex_memory_search" => {
                self.search_knowledge(principal_id, request).await
            }
            "cortex_task_list" => self.list_tasks(principal_id, request).await,
            "cortex_agent_run" => self.run_agent(principal_id, request).await,
            "cortex_knowledge_create"
            | "cortex_knowledge_update"
            | "cortex_knowledge_delete"
            | "cortex_task_create"
            | "cortex_task_update"
            | "cortex_task_complete"
            | "cortex_task_delete"
            | "cortex_task_restore"
            | "cortex_memory_create"
            | "cortex_memory_correct"
            | "cortex_memory_delete"
            | "cortex_memory_restore" => {
                let response = self.dispatch_mutation(principal_id, request).await?;
                // Provider mutations changed vault content: refresh the
                // derived index so retrieval and health stay current (the
                // index lag for user surfaces is one mutation, not a scan
                // interval). A failed refresh keeps the previous index and
                // marks health stale; it never fails the mutation itself.
                let _ = self.refresh_vault_index();
                self.refresh_embedding(&response).await;
                Ok(response)
            }
            _ => Err(DaemonError::UnsupportedCapability),
        }
    }

    async fn enroll_remote_principal(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct EnrollmentPayload {
            subject: String,
            grants: Vec<String>,
        }

        let context = self.command_context(principal_id, request)?;
        if principal_id != self.owner_principal_id {
            self.append_administration_audit(&context, principal_id, AuditResult::Rejected)
                .await?;
            return Err(DaemonError::PermissionDenied);
        }
        let payload: EnrollmentPayload = decode_payload(&request.payload)?;
        let grants = payload
            .grants
            .iter()
            .map(|grant| Capability::from_mcp_name(grant).ok_or(DaemonError::InvalidRequest))
            .collect::<Result<Vec<_>, _>>()?;
        let config = DaemonConfig::from_database_path(self.database_path.clone())?;
        let proposed_principal_id = PrincipalId::new();
        let record = self
            .database
            .operation_store()
            .enroll_remote_once(RemoteEnrollmentRequest {
                workspace_id: self.workspace_id,
                owner_principal_id: self.owner_principal_id,
                operation_id: context.operation_id,
                correlation_id: context.correlation_id,
                subject: payload.subject,
                principal_id: proposed_principal_id,
                grants,
                pairing_verifier: config
                    .derived_remote_signing_key(proposed_principal_id)
                    .verifying_key()
                    .to_bytes(),
                max_remote_clients: u8::try_from(MAX_REMOTE_CLIENTS)
                    .map_err(|_| DaemonError::InvalidConfiguration)?,
            })
            .await
            .map_err(DaemonError::from)?;
        let mut reconciliation_config =
            DaemonConfig::from_database_path(self.database_path.clone())?;
        let enrollment = reconciliation_config.reconcile_remote_enrollment(&record)?;
        Ok(DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: WireResult::Success {
                value: json!({
                    "correlation_id": record.correlation_id.to_string(),
                    "principal_id": Uuid::from(enrollment.principal_id).to_string(),
                    "subject": enrollment.subject,
                    "grants": enrollment.grants.iter().map(|grant| grant.metadata().mcp_name).collect::<Vec<_>>(),
                    "ipc_enrollment_path": enrollment.enrollment_path,
                    "gateway_paired_subject": {
                        "subject": enrollment.subject,
                        "principal_id": Uuid::from(enrollment.principal_id).to_string(),
                        "ipc_enrollment_path": enrollment.enrollment_path,
                    },
                    "restart_required": true
                }),
            },
        })
    }

    async fn append_administration_audit(
        &self,
        context: &CommandContext,
        principal_id: PrincipalId,
        result: AuditResult,
    ) -> Result<(), DaemonError> {
        self.audit
            .append(AuditEvent {
                id: AuditEventId::new(),
                workspace_id: self.workspace_id,
                principal_id,
                operation_id: context.operation_id,
                correlation_id: context.correlation_id,
                capability: "cortex_remote_enroll",
                target: None,
                provider_metadata: None,
                policy_decision: if principal_id == self.owner_principal_id {
                    PolicyDecision::Allow
                } else {
                    PolicyDecision::Deny(PolicyDeny::MissingGrant)
                },
                result,
            })
            .await
            .map_err(DaemonError::from)
    }

    async fn create_knowledge_document(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        let input: WireKnowledgeCreate = serde_json::from_value(request.payload.clone())
            .map_err(|_| DaemonError::InvalidRequest)?;
        let context = self.command_context(principal_id, request)?;
        let authority = self
            .authority
            .as_ref()
            .ok_or(DaemonError::InvalidConfiguration)?;
        let create = KnowledgeCreate::new(
            context.workspace_id,
            context.operation_id,
            input.title,
            input.content,
        )
        .map_err(|_| DaemonError::InvalidRequest)?;
        let outcome = authority
            .create_knowledge(
                &context,
                Capability::from_mcp_name(&request.capability)
                    .ok_or(DaemonError::UnsupportedCapability)?,
                create,
            )
            .await
            .map_err(DaemonError::from)?;
        Ok(provider_mutation_response(request.request_id, &outcome))
    }

    /// The provider resource reference for a wire resource id of the given
    /// kind: the daemon's configured vault is the only first-party provider.
    fn resource_for(
        &self,
        kind: ProviderResourceKind,
        resource_id: &cortex_domain::ProviderResourceId,
    ) -> Result<ProviderResourceRef, DaemonError> {
        let vault = self
            .vault
            .as_ref()
            .ok_or(DaemonError::InvalidConfiguration)?;
        Ok(ProviderResourceRef::new(
            self.workspace_id,
            vault.provider_reference_id().clone(),
            resource_id.clone(),
            kind,
        ))
    }

    /// Reads the current task content for a restore, which must rewrite the
    /// whole task record. Policy is enforced on the mutation itself.
    async fn read_task(&self, resource: &ProviderResourceRef) -> Result<ProviderTask, DaemonError> {
        let vault = self
            .vault
            .as_ref()
            .ok_or(DaemonError::InvalidConfiguration)?;
        Ok(TaskProvider::get(vault.as_ref(), resource)
            .await
            .map_err(|_error| DaemonError::from(ApplicationError::Internal))?
            .filter(|read| read.freshness() == cortex_application::ProviderFreshness::Current)
            .ok_or(DaemonError::NotFound)?
            .into_item())
    }

    #[allow(clippy::too_many_lines)] // Exhaustive, typed catalog-to-service mapping stays auditable in one place.
    async fn dispatch_mutation(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        if request.capability == "cortex_knowledge_create" {
            return self.create_knowledge_document(principal_id, request).await;
        }
        let context = self.command_context(principal_id, request)?;
        let result: MutationOutcome = match request.capability.as_str() {
            "cortex_knowledge_update" => {
                let input: WireKnowledgeUpdate = decode_payload(&request.payload)?;
                let (resource_id, expected_revision) = input.resource()?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let update = KnowledgeUpdate::new(
                    self.resource_for(ProviderResourceKind::Knowledge, &resource_id)?,
                    context.operation_id,
                    expected_revision,
                    input.title,
                    input.content,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .update_knowledge(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        update,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_knowledge_delete" => {
                let input: WireResourceCommand = decode_payload(&request.payload)?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let delete = KnowledgeDelete::new(
                    self.resource_for(ProviderResourceKind::Knowledge, &input.resource_id()?)?,
                    context.operation_id,
                    input.expected_revision()?,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .delete_knowledge(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        delete,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_task_create" => {
                let input: WireTaskCreate = decode_payload(&request.payload)?;
                let due_at = input.due_at()?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let create = TaskCreate::new(
                    context.workspace_id,
                    context.operation_id,
                    TaskId::new(),
                    input.title,
                    String::new(),
                    cortex_application::ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::new(
                        due_at,
                        None,
                        None,
                        None,
                        None,
                        None::<String>,
                        None::<String>,
                    )
                    .map_err(|_| DaemonError::InvalidRequest)?,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .create_task(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        create,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_task_update" => {
                let input: WireTaskUpdate = decode_payload(&request.payload)?;
                let due_at = input.due_at()?;
                let (resource_id, expected_revision) = input.resource()?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let update = TaskUpdate::new(
                    self.resource_for(ProviderResourceKind::Task, &resource_id)?,
                    context.operation_id,
                    expected_revision,
                    input.title,
                    String::new(),
                    cortex_application::ProviderTaskStatus::Todo,
                    cortex_application::ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::new(
                        due_at,
                        None,
                        None,
                        None,
                        None,
                        None::<String>,
                        None::<String>,
                    )
                    .map_err(|_| DaemonError::InvalidRequest)?,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .update_task(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        update,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_task_complete" => {
                let input: WireResourceCommand = decode_payload(&request.payload)?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let complete = TaskComplete::new(
                    self.resource_for(ProviderResourceKind::Task, &input.resource_id()?)?,
                    context.operation_id,
                    input.expected_revision()?,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .complete_task(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        complete,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_task_delete" => {
                let input: WireResourceCommand = decode_payload(&request.payload)?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let delete = TaskDelete::new(
                    self.resource_for(ProviderResourceKind::Task, &input.resource_id()?)?,
                    context.operation_id,
                    input.expected_revision()?,
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .delete_task(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        delete,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_task_restore" => {
                let input: WireResourceCommand = decode_payload(&request.payload)?;
                let authority = self
                    .authority
                    .as_ref()
                    .ok_or(DaemonError::InvalidConfiguration)?;
                let resource =
                    self.resource_for(ProviderResourceKind::Task, &input.resource_id()?)?;
                // Policy is evaluated before any provider content is read: a
                // restore must not observe vault state it may not rewrite.
                let capability = Capability::from_mcp_name(&request.capability)
                    .ok_or(DaemonError::UnsupportedCapability)?;
                if let Some(authority) = self.authority.as_ref() {
                    authority
                        .authorize(
                            &context,
                            capability,
                            cortex_domain::ResourceTarget::ProviderResource(resource.clone()),
                        )
                        .await
                        .map_err(DaemonError::from)?;
                }
                // A restore rewrites the whole record: the current content is
                // read, while the caller's revision governs the write.
                let current = self.read_task(&resource).await?;
                let restore = TaskUpdate::new(
                    resource,
                    context.operation_id,
                    input.expected_revision()?,
                    current.title().to_owned(),
                    current.body().to_owned(),
                    cortex_application::ProviderTaskStatus::Todo,
                    current.priority(),
                    current.scheduling().clone(),
                )
                .map_err(|_| DaemonError::InvalidRequest)?;
                authority
                    .update_task(
                        &context,
                        Capability::from_mcp_name(&request.capability)
                            .ok_or(DaemonError::UnsupportedCapability)?,
                        restore,
                    )
                    .await
                    .map(MutationOutcome::Provider)
            }
            "cortex_memory_create" => {
                let input: WireMemoryCreate = decode_payload(&request.payload)?;
                self.service
                    .create_memory(context, input.into_input()?)
                    .await
                    .map(MutationOutcome::Legacy)
            }
            "cortex_memory_correct" => {
                let input: WireMemoryCorrect = decode_payload(&request.payload)?;
                let entity = input.entity_id()?;
                let revision = input.revision()?;
                self.service
                    .correct_memory(context, entity, revision, input.into_input()?)
                    .await
                    .map(MutationOutcome::Legacy)
            }
            "cortex_memory_delete" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .delete_memory(context, input.memory_entity()?, input.memory_revision()?)
                    .await
                    .map(MutationOutcome::Legacy)
            }
            "cortex_memory_restore" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .restore_memory(context, input.memory_entity()?, input.memory_revision()?)
                    .await
                    .map(MutationOutcome::Legacy)
            }
            _ => return Err(DaemonError::UnsupportedCapability),
        }
        .map_err(DaemonError::from)?;
        match result {
            MutationOutcome::Provider(outcome) => {
                Ok(provider_mutation_response(request.request_id, &outcome))
            }
            MutationOutcome::Legacy(result) => Ok(mutation_response(request.request_id, result)),
        }
    }

    async fn search_knowledge(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SearchPayload {
            query: String,
            limit: Option<usize>,
        }
        let capability = Capability::from_mcp_name(&request.capability)
            .ok_or(DaemonError::UnsupportedCapability)?;
        self.authorize_and_audit(principal_id, request, capability, AuditResult::Succeeded)
            .await?;
        let payload: SearchPayload = decode_payload(&request.payload)?;
        let limit = std::num::NonZeroUsize::new(payload.limit.unwrap_or(20).min(100))
            .ok_or(DaemonError::InvalidRequest)?;
        // Cortex-owned AI memories come from the SQLite-backed hybrid search;
        // user-authored knowledge comes from provenance-bearing vault chunks.
        let memory_hits = self
            .search
            .search(SearchRequest {
                workspace_id: self.workspace_id,
                principal_id,
                query: payload.query.clone(),
                limit,
            })
            .await
            .map_err(DaemonError::from)?
            .into_iter()
            .filter(|hit| hit.kind == cortex_search::EntityKind::Memory)
            .collect::<Vec<SearchHit>>();
        let vault_outcome = self.vault_retrieval(&payload.query, limit).await;
        let fused = fuse_with_memories(vault_outcome, memory_hits, limit);
        Ok(fused_search_response(request.request_id, fused))
    }

    /// Lexical/semantic retrieval over the derived vault index. Embedding
    /// unavailability degrades to lexical-only instead of failing the search.
    async fn vault_retrieval(
        &self,
        query: &str,
        limit: std::num::NonZeroUsize,
    ) -> RetrievalOutcome {
        let query_embedding =
            cortex_application::EmbeddingProvider::embed(&self.embedding_provider, query)
                .await
                .ok();
        let index = self.vault_index.read().expect("vault index mutex poisoned");
        vault_hybrid(&index, query, query_embedding.as_ref(), limit).unwrap_or(RetrievalOutcome {
            hits: Vec::new(),
            semantic_degraded: true,
        })
    }

    async fn list_tasks(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TaskListPayload {
            limit: Option<usize>,
        }
        self.authorize_and_audit(
            principal_id,
            request,
            Capability::TaskList,
            AuditResult::Succeeded,
        )
        .await?;
        let payload: TaskListPayload = decode_payload(&request.payload)?;
        let limit = std::num::NonZeroUsize::new(payload.limit.unwrap_or(20).min(100))
            .ok_or(DaemonError::InvalidRequest)?;
        let authority = self
            .authority
            .as_ref()
            .ok_or(DaemonError::InvalidConfiguration)?;
        let query = TaskQuery::new(self.workspace_id, None::<String>, limit)
            .map_err(|_| DaemonError::InvalidRequest)?;
        let context = CommandContext::from_authenticated(
            self.workspace_id,
            principal_id,
            OperationId::new(),
            request.request_id,
        );
        let page = authority
            .list_tasks(&context, query)
            .await
            .map_err(DaemonError::from)?;
        let rows: Vec<Value> = page
            .items()
            .iter()
            .map(|task| {
                json!({
                    "task_id": Uuid::from(task.task_id()).to_string(),
                    "resource_id": task.provenance().resource().resource_id().as_str(),
                    "title": task.title(),
                    "due_at": task.scheduling().due_at().map(|due| due.to_rfc3339()),
                    "status": format!("{:?}", task.status()).to_ascii_lowercase(),
                    "priority": format!("{:?}", task.priority()).to_ascii_lowercase(),
                    "revision": task.provenance().observed_revision().as_str(),
                })
            })
            .collect();
        Ok(DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: WireResult::Success {
                value: json!({
                    "freshness": match page.freshness() {
                        cortex_application::ProviderFreshness::Current => "current",
                        cortex_application::ProviderFreshness::Stale => "stale",
                    },
                    "tasks": rows,
                }),
            },
        })
    }

    async fn run_agent(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        self.run_agent_with_partials(principal_id, request, None)
            .await
    }

    /// Streaming form of `cortex_agent_run`: every assistant text segment the
    /// bounded loop produces is forwarded through `partials` as an
    /// intermediate frame; the returned response is the terminal frame.
    /// Authorization, auditing, and loop bounds are identical to the
    /// non-streaming path.
    async fn run_agent_with_partials(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
        partials: Option<tokio::sync::mpsc::UnboundedSender<DaemonResponse>>,
    ) -> Result<DaemonResponse, DaemonError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AgentPayload {
            prompt: String,
            // Opt-in marker routed by the session layer; read only for the
            // wire contract, not for dispatch decisions.
            #[allow(dead_code)]
            stream: Option<bool>,
        }
        self.authorize_and_audit(
            principal_id,
            request,
            Capability::AgentRun,
            AuditResult::Succeeded,
        )
        .await?;
        let payload: AgentPayload = decode_payload(&request.payload)?;
        let allowed = AuthorizedCapabilities::new(CapabilityCatalog::all().iter().copied().filter(
            |capability| {
                *capability != Capability::AgentRun
                    && self.grants.contains(&CapabilityGrant::new(
                        self.workspace_id,
                        principal_id,
                        *capability,
                    ))
            },
        ))
        .map_err(DaemonError::from)?;
        let context = self.command_context(principal_id, request)?;
        let limits = AgentLimits::new(4, Duration::from_secs(30), 1, 32 * 1024, 128 * 1024)
            .map_err(DaemonError::from)?;
        // SCRUM-79 + SCRUM-147: the Stable tier composes deterministically
        // from Cortex's protected instructions plus the configured Brain
        // prompt layers for the resolved profile. It is a pure function of
        // the process-lifetime prompt configuration, so it stays
        // byte-identical across the turns of a session; configuration
        // changes apply at the next daemon start. Retrieved memories, vault
        // content and tool results remain contextual data in the volatile
        // tiers and cannot elevate into this Stable tier.
        let resolved_profile = self
            .resolved_profile
            .read()
            .ok()
            .and_then(|guard| guard.clone());
        let layers = PromptLayers::new(PROTECTED_CORTEX_PROMPT)
            .map_err(DaemonError::from)?
            .with_user_global(self.prompt.global.clone())
            .map_err(DaemonError::from)?
            .with_profile(
                resolved_profile
                    .as_deref()
                    .and_then(|id| self.prompt.profiles.get(id).cloned()),
            )
            .map_err(DaemonError::from)?;
        let system_prompt = layers.into_system_prompt().map_err(DaemonError::from)?;
        let agent = AgentRunner::new(
            Arc::new(self.embedding_provider.clone()),
            Arc::new(DaemonAgentExecutor {
                daemon: self.clone(),
            }),
            limits,
        )
        .with_system_prompt(system_prompt)
        .with_compaction_threshold(limits.max_request_bytes / 2)
        .map_err(DaemonError::from)?;
        let output = match partials {
            Some(sender) => {
                let request_id = request.request_id;
                let forward = move |chunk: &str| {
                    let _ = sender.send(DaemonResponse {
                        protocol_version: PROTOCOL_VERSION,
                        request_id,
                        result: WireResult::Success {
                            value: json!({ "partial": chunk }),
                        },
                    });
                };
                agent
                    .run_streaming(context, &payload.prompt, allowed, &forward)
                    .await
            }
            None => agent.run(context, &payload.prompt, allowed).await,
        }
        .map_err(DaemonError::from)?;
        Ok(DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: WireResult::Success {
                value: json!({ "content": output }),
            },
        })
    }

    async fn refresh_embedding(&self, response: &DaemonResponse) {
        let WireResult::Success { value } = &response.result else {
            return;
        };
        let Some(entity_id) = value
            .get("entity_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .and_then(|value| EntityId::try_from(value).ok())
        else {
            return;
        };
        let repositories = self.database.repositories();
        let Ok(Some(text)) = repositories
            .search_document_text(self.workspace_id, entity_id)
            .await
        else {
            return;
        };
        let Ok(embedding) =
            cortex_application::EmbeddingProvider::embed(&self.embedding_provider, &text).await
        else {
            return;
        };
        let _ = repositories
            .upsert_embedding(
                self.workspace_id,
                entity_id,
                embedding.model_id(),
                embedding.model_version(),
                embedding.dimensions(),
                &embedding.to_le_bytes(),
            )
            .await;
    }

    async fn authorize_and_audit(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
        capability: Capability,
        allowed_result: AuditResult,
    ) -> Result<(), DaemonError> {
        let context = self.command_context(principal_id, request)?;
        let allowed = self.grants.contains(&CapabilityGrant::new(
            self.workspace_id,
            principal_id,
            capability,
        ));
        let decision = if allowed {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(PolicyDeny::MissingGrant)
        };
        let result = if allowed {
            allowed_result
        } else {
            AuditResult::Rejected
        };
        self.audit
            .append(AuditEvent {
                id: AuditEventId::new(),
                workspace_id: self.workspace_id,
                principal_id,
                operation_id: context.operation_id,
                correlation_id: context.correlation_id,
                capability: capability.metadata().mcp_name,
                target: None,
                provider_metadata: None,
                policy_decision: decision,
                result,
            })
            .await
            .map_err(DaemonError::from)?;
        if allowed {
            Ok(())
        } else {
            Err(DaemonError::PermissionDenied)
        }
    }

    fn command_context(
        &self,
        principal_id: PrincipalId,
        request: &DaemonRequest,
    ) -> Result<CommandContext, DaemonError> {
        Ok(CommandContext::from_authenticated(
            self.workspace_id,
            principal_id,
            OperationId::try_from(request.operation_id).map_err(|_| DaemonError::InvalidRequest)?,
            request.request_id,
        ))
    }

    /// Applies the daemon-owned authenticated principal after a transport challenge succeeded.
    #[must_use]
    pub async fn handle_authenticated_request(&self, request: DaemonRequest) -> DaemonResponse {
        self.handle_request_for(self.owner_principal_id, request)
            .await
    }

    async fn handle_request_for(
        &self,
        principal_id: PrincipalId,
        request: DaemonRequest,
    ) -> DaemonResponse {
        let request_id = request.request_id;
        let result = match self.request_authenticated(principal_id, &request).await {
            Ok(response) => return response,
            Err(error) => WireResult::Error {
                code: error.wire_code().to_owned(),
            },
        };
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result,
        }
    }

    fn pairing_response(&self, challenge: &PairingChallenge) -> PairingResponse {
        pairing_response_for(&self.owner_pairing_signer, challenge)
    }

    fn verify_pairing(
        &self,
        challenge: &PairingChallenge,
        response: &PairingResponse,
    ) -> Option<PrincipalId> {
        if response.protocol_version != PROTOCOL_VERSION || response.signature.len() != 64 {
            return None;
        }
        let Ok(signature) = Signature::try_from(response.signature.as_slice()) else {
            return None;
        };
        self.client_verifiers.iter().find_map(|client| {
            client
                .pairing_verifier
                .verify(&pairing_message(challenge), &signature)
                .is_ok()
                .then_some(client.principal_id)
        })
    }

    /// The discovered model catalog for client-side model selection.
    /// Empty while background resolution is pending, disabled, or degraded.
    fn model_list_response(&self, request_id: Uuid) -> DaemonResponse {
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Success {
                value: json!({
                    "models": self.discovered_models(),
                }),
            },
        }
    }

    fn diagnostic_response(
        &self,
        principal_id: PrincipalId,
        request_id: Uuid,
        capability: &str,
    ) -> DaemonResponse {
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Success {
                value: json!({
                    "capability": capability,
                    "correlation_id": request_id.to_string(),
                    "workspace_id": Uuid::from(self.workspace_id).to_string(),
                    "principal_id": Uuid::from(principal_id).to_string(),
                    "migrations_applied": self.migrations_applied,
                    "vault": self.vault_health_json(),
                }),
            },
        }
    }

    /// Secret-free vault health for doctor/status: configuration facts
    /// (root path, scopes, mode), accessibility, and derived-index state.
    /// Never includes vault content, identities, or credentials.
    fn vault_health_json(&self) -> Value {
        let Some(config) = &self.vault_config else {
            return json!({ "configured": false });
        };
        let health = self
            .vault_health
            .read()
            .expect("vault health mutex poisoned")
            .clone();
        // Accessibility is evaluated live so a sync outage (mount loss,
        // deleted root) is visible in diagnostics without waiting for the
        // next refresh.
        let root_accessible = config.validate_root_access().is_ok();
        let fresh = health.fresh && root_accessible;
        let mut scopes: Vec<&str> = config
            .scopes()
            .iter()
            .map(|scope| match scope.resource_kind() {
                cortex_domain::ProviderResourceKind::Knowledge => "knowledge",
                cortex_domain::ProviderResourceKind::Task => "task",
            })
            .collect();
        scopes.sort_unstable();
        json!({
            "configured": true,
            "provider_id": self.vault.as_ref().map_or("", |v| v.provider_reference_id().as_str()),
            "root": config.root().display().to_string(),
            "mode": match config.mode() {
                crate::vault::VaultProviderMode::ReadOnly => "read_only",
                crate::vault::VaultProviderMode::ReadWrite => "read_write",
            },
            "scopes": scopes,
            "root_accessible": root_accessible,
            "fresh": fresh,
            "index": {
                "refreshed_at": health.refreshed_at,
                "indexed": health.indexed,
                "unchanged": health.unchanged,
                "removed": health.removed,
                "skipped": health.skipped,
            },
            "semantic": if self.embedding_available() { "available" } else { "degraded" },
        })
    }

    fn embedding_available(&self) -> bool {
        matches!(
            self.embedding_provider.current(),
            DaemonEmbeddingProvider::Configured(_)
        )
    }

    /// Serves authenticated, per-user local IPC until `shutdown` is signalled.
    ///
    /// Windows uses a local named pipe with remote clients rejected. Unix uses a socket mode
    /// restricted to the owning user. Neither branch binds an Internet-facing listener.
    ///
    /// # Errors
    /// Returns a safe transport failure if the local endpoint cannot bind or serve.
    pub async fn serve(
        self: Arc<Self>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), DaemonError> {
        serve_platform(self, shutdown).await
    }
}

fn pairing_response_for(
    pairing_signer: &ed25519_dalek::SigningKey,
    challenge: &PairingChallenge,
) -> PairingResponse {
    PairingResponse {
        protocol_version: PROTOCOL_VERSION,
        signature: pairing_signer
            .sign(&pairing_message(challenge))
            .to_bytes()
            .to_vec(),
    }
}

fn validate_request(request: &DaemonRequest) -> Result<(), DaemonError> {
    if request.protocol_version != PROTOCOL_VERSION
        || !is_v7(request.request_id)
        || !is_v7(request.principal_id)
        || !is_v7(request.operation_id)
        || request.capability.trim().is_empty()
        || request.capability.len() > 128
        || serde_json::to_vec(&request.payload)
            .map_err(|_| DaemonError::InvalidRequest)?
            .len()
            > MAX_FRAME_BYTES
    {
        return Err(DaemonError::InvalidRequest);
    }
    Ok(())
}

fn is_v7(value: Uuid) -> bool {
    value.get_version() == Some(Version::SortRand)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireKnowledgeCreate {
    title: String,
    content: String,
}

/// A provider resource address plus the opaque observed revision the caller
/// based its mutation on. Bounds are enforced by the domain constructors.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResourceCommand {
    resource_id: String,
    expected_revision: String,
}

impl WireResourceCommand {
    fn resource_id(&self) -> Result<cortex_domain::ProviderResourceId, DaemonError> {
        cortex_domain::ProviderResourceId::new(self.resource_id.clone())
            .map_err(|_| DaemonError::InvalidRequest)
    }
    fn expected_revision(&self) -> Result<ObservedRevision, DaemonError> {
        ObservedRevision::new(self.expected_revision.clone())
            .map_err(|_| DaemonError::InvalidRequest)
    }
}

/// A provider note mutation carrying content plus the opaque observed
/// revision the caller based the write on.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireKnowledgeUpdate {
    resource_id: String,
    expected_revision: String,
    title: String,
    content: String,
}

impl WireKnowledgeUpdate {
    fn resource(
        &self,
    ) -> Result<(cortex_domain::ProviderResourceId, ObservedRevision), DaemonError> {
        Ok((
            cortex_domain::ProviderResourceId::new(self.resource_id.clone())
                .map_err(|_| DaemonError::InvalidRequest)?,
            ObservedRevision::new(self.expected_revision.clone())
                .map_err(|_| DaemonError::InvalidRequest)?,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntityCommand {
    entity_id: Uuid,
    expected_revision: u64,
}

impl WireEntityCommand {
    fn memory_entity(&self) -> Result<EntityId, DaemonError> {
        EntityId::try_from(self.entity_id).map_err(|_| DaemonError::InvalidRequest)
    }
    fn memory_revision(&self) -> Result<Revision, DaemonError> {
        Revision::rehydrate(self.expected_revision).map_err(|_| DaemonError::InvalidRequest)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTaskCreate {
    title: String,
    due_at: Option<String>,
}

impl WireTaskCreate {
    fn due_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>, DaemonError> {
        self.due_at
            .as_deref()
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(value)
                    .map(|date| date.with_timezone(&chrono::Utc))
                    .map_err(|_| DaemonError::InvalidRequest)
            })
            .transpose()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTaskUpdate {
    resource_id: String,
    expected_revision: String,
    title: String,
    due_at: Option<String>,
}

impl WireTaskUpdate {
    fn resource(
        &self,
    ) -> Result<(cortex_domain::ProviderResourceId, ObservedRevision), DaemonError> {
        Ok((
            cortex_domain::ProviderResourceId::new(self.resource_id.clone())
                .map_err(|_| DaemonError::InvalidRequest)?,
            ObservedRevision::new(self.expected_revision.clone())
                .map_err(|_| DaemonError::InvalidRequest)?,
        ))
    }

    fn due_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>, DaemonError> {
        self.due_at
            .as_deref()
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(value)
                    .map(|date| date.with_timezone(&chrono::Utc))
                    .map_err(|_| DaemonError::InvalidRequest)
            })
            .transpose()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSource {
    source_id: Uuid,
}

fn source_refs(sources: Vec<WireSource>) -> Result<Vec<SourceRef>, DaemonError> {
    sources
        .into_iter()
        .map(|source| {
            EntityId::try_from(source.source_id)
                .map(|source_id| SourceRef { source_id })
                .map_err(|_| DaemonError::InvalidRequest)
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMemoryCreate {
    statement: String,
    normalized_subject: String,
    normalized_predicate: String,
    normalized_object: String,
    sources: Vec<WireSource>,
}
impl WireMemoryCreate {
    fn into_input(self) -> Result<MemoryCreateInput, DaemonError> {
        Ok(MemoryCreateInput {
            statement: self.statement,
            normalized_subject: self.normalized_subject,
            normalized_predicate: self.normalized_predicate,
            normalized_object: self.normalized_object,
            sources: source_refs(self.sources)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMemoryCorrect {
    entity_id: Uuid,
    expected_revision: u64,
    statement: String,
    normalized_subject: String,
    normalized_predicate: String,
    normalized_object: String,
    sources: Vec<WireSource>,
}
impl WireMemoryCorrect {
    fn entity_id(&self) -> Result<EntityId, DaemonError> {
        EntityId::try_from(self.entity_id).map_err(|_| DaemonError::InvalidRequest)
    }
    fn revision(&self) -> Result<Revision, DaemonError> {
        Revision::rehydrate(self.expected_revision).map_err(|_| DaemonError::InvalidRequest)
    }
    fn into_input(self) -> Result<MemoryCorrectInput, DaemonError> {
        Ok(MemoryCorrectInput {
            statement: self.statement,
            normalized_subject: self.normalized_subject,
            normalized_predicate: self.normalized_predicate,
            normalized_object: self.normalized_object,
            sources: source_refs(self.sources)?,
        })
    }
}

fn decode_payload<T>(payload: &Value) -> Result<T, DaemonError>
where
    T: for<'a> Deserialize<'a>,
{
    serde_json::from_value(payload.clone()).map_err(|_| DaemonError::InvalidRequest)
}

fn mutation_response(
    request_id: Uuid,
    result: cortex_application::MutationResult,
) -> DaemonResponse {
    DaemonResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: WireResult::Success {
            value: json!({ "entity_id": Uuid::from(result.entity_id).to_string(), "revision": result.revision.get(), "lifecycle": format!("{:?}", result.lifecycle).to_ascii_lowercase(), "correlation_id": result.audit_correlation_id.to_string() }),
        },
    }
}

/// Wire shape for provider-backed mutations: resource identity and opaque
/// observed revisions replace the legacy numeric entity contract.
fn provider_mutation_response(
    request_id: Uuid,
    outcome: &ProviderMutationOutcome,
) -> DaemonResponse {
    let revision = |value: Option<&ObservedRevision>| {
        value.map(|revision| json!({ "revision": revision.as_str() }))
    };
    DaemonResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: WireResult::Success {
            value: json!({
                "resource": {
                    "provider_id": outcome.resource.provider_id().as_str(),
                    "resource_id": outcome.resource.resource_id().as_str(),
                    "kind": match outcome.resource.kind() {
                        ProviderResourceKind::Knowledge => "knowledge",
                        ProviderResourceKind::Task => "task",
                    },
                },
                "previous_revision": revision(outcome.previous_revision.as_ref()),
                "revision": revision(outcome.current_revision.as_ref()),
            }),
        },
    }
}

/// Fused retrieval response: vault chunk hits carry their provider resource
/// reference and chunk ordinal (provenance), memory hits keep the legacy
/// entity shape.
fn fused_search_response(request_id: Uuid, hits: Vec<cortex_search::FusedHit>) -> DaemonResponse {
    let values: Vec<_> = hits
        .into_iter()
        .take(MAX_SEARCH_RESULTS)
        .map(|hit| match hit.leg {
            FusedLeg::VaultChunk(chunk) => json!({
                "kind": "vault_chunk",
                "provider": chunk.reference.resource.provider_id().as_str(),
                "resource_id": chunk.reference.resource.resource_id().as_str(),
                "resource_kind": match chunk.reference.resource.kind() {
                    cortex_domain::ProviderResourceKind::Knowledge => "knowledge",
                    cortex_domain::ProviderResourceKind::Task => "task",
                },
                "chunk": chunk.reference.chunk.get(),
                "snippet": truncate_utf8(&chunk.snippet, MAX_SEARCH_SNIPPET_BYTES),
                "semantic_degraded": chunk.semantic_rank.is_none(),
            }),
            FusedLeg::Memory(hit) => json!({
                "entity_id": Uuid::from(hit.entity_id).to_string(),
                "kind": hit.kind.as_str(),
                "snippet": truncate_utf8(&hit.snippet, MAX_SEARCH_SNIPPET_BYTES),
                "sources": hit.sources.into_iter().map(|source| Uuid::from(source.source_id).to_string()).collect::<Vec<_>>(),
                "semantic_degraded": hit.semantic_degraded,
            }),
        })
        .collect();
    DaemonResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: WireResult::Success {
            value: Value::Array(values),
        },
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

async fn process_stream<S>(daemon: &LocalDaemon, stream: &mut S) -> Result<(), DaemonError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let challenge = PairingChallenge {
        protocol_version: PROTOCOL_VERSION,
        nonce: Uuid::now_v7(),
    };
    write_json_frame(stream, &challenge).await?;
    let pairing = read_frame(stream).await?;
    let pairing: PairingResponse =
        serde_json::from_slice(&pairing).map_err(|_| DaemonError::Unauthenticated)?;
    let principal_id = daemon
        .verify_pairing(&challenge, &pairing)
        .ok_or(DaemonError::Unauthenticated)?;
    let bytes = read_frame(stream).await?;
    if requests_streaming_agent_output(&bytes) {
        return stream_agent_response(daemon, principal_id, stream, &bytes).await;
    }
    let response = response_for_request(daemon, principal_id, &bytes).await?;
    write_response(stream, &response).await
}

/// True unless the frame explicitly opts out of streaming with
/// `"stream": false`. Only `cortex_agent_run` streams; every other
/// capability stays on the unchanged single-response path.
fn requests_streaming_agent_output(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Peek {
        capability: String,
        payload: Value,
    }
    serde_json::from_slice::<Peek>(bytes).is_ok_and(|peek| {
        peek.capability == "cortex_agent_run"
            && peek.payload.get("stream").and_then(Value::as_bool) != Some(false)
    })
}

/// Forwards intermediate agent frames as they are produced, then writes the
/// terminal response. All frames share the request id and stay within the
/// bounded-frame discipline; the stream terminates explicitly with the final
/// frame, exactly like the non-streaming path.
async fn stream_agent_response<S>(
    daemon: &LocalDaemon,
    principal_id: PrincipalId,
    stream: &mut S,
    bytes: &[u8],
) -> Result<(), DaemonError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = daemon.decode_request(bytes)?;
    let request_id = request.request_id;
    let (partials_sender, mut partials_receiver) =
        tokio::sync::mpsc::unbounded_channel::<DaemonResponse>();
    let daemon = daemon.clone();
    let runner = tokio::spawn(async move {
        daemon
            .run_agent_with_partials(principal_id, &request, Some(partials_sender))
            .await
    });
    while let Some(frame) = partials_receiver.recv().await {
        write_json_frame(stream, &frame).await?;
    }
    let response = match runner.await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Error {
                code: error.wire_code().to_owned(),
            },
        },
        Err(_) => DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Error {
                code: DaemonError::TransportUnavailable.wire_code().to_owned(),
            },
        },
    };
    write_response(stream, &response).await
}

async fn response_for_request(
    daemon: &LocalDaemon,
    principal_id: PrincipalId,
    bytes: &[u8],
) -> Result<DaemonResponse, DaemonError> {
    let response = match daemon.decode_request(bytes) {
        Ok(request) => daemon.handle_request_for(principal_id, request).await,
        Err(error) => {
            let request_id = recover_request_id(bytes).ok_or(DaemonError::InvalidRequest)?;
            DaemonResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                result: WireResult::Error {
                    code: error.wire_code().to_owned(),
                },
            }
        }
    };
    Ok(response)
}

async fn read_frame<S>(stream: &mut S) -> Result<Vec<u8>, DaemonError>
where
    S: AsyncRead + Unpin,
{
    let length = stream
        .read_u32_le()
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    let length = usize::try_from(length).map_err(|_| DaemonError::InvalidRequest)?;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(DaemonError::InvalidRequest);
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    Ok(bytes)
}

fn recover_request_id(bytes: &[u8]) -> Option<Uuid> {
    #[derive(Deserialize)]
    struct RequestId {
        request_id: Uuid,
    }
    let request_id = serde_json::from_slice::<RequestId>(bytes).ok()?.request_id;
    is_v7(request_id).then_some(request_id)
}

async fn write_response<S>(stream: &mut S, response: &DaemonResponse) -> Result<(), DaemonError>
where
    S: AsyncWrite + Unpin,
{
    let response = if serialized_len(response).is_ok_and(|length| length <= MAX_FRAME_BYTES) {
        response.clone()
    } else {
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: response.request_id,
            result: WireResult::Error {
                code: "response_too_large".to_owned(),
            },
        }
    };
    write_json_frame(stream, &response).await
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, DaemonError> {
    serde_json::to_vec(value)
        .map(|value| value.len())
        .map_err(|_| DaemonError::TransportUnavailable)
}

async fn write_json_frame<S, T>(stream: &mut S, value: &T) -> Result<(), DaemonError>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value).map_err(|_| DaemonError::TransportUnavailable)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(DaemonError::TransportUnavailable);
    }
    let length = u32::try_from(bytes.len()).map_err(|_| DaemonError::TransportUnavailable)?;
    stream
        .write_u32_le(length)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|_| DaemonError::TransportUnavailable)?;
    stream
        .flush()
        .await
        .map_err(|_| DaemonError::TransportUnavailable)
}

fn pairing_message(challenge: &PairingChallenge) -> Vec<u8> {
    let mut message = b"cortexd-local-ipc-pairing-v1\0".to_vec();
    message.extend_from_slice(&challenge.protocol_version.to_le_bytes());
    message.extend_from_slice(challenge.nonce.as_bytes());
    message
}

#[cfg(windows)]
async fn serve_platform(
    daemon: Arc<LocalDaemon>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    use crate::windows_security::create_current_user_server;
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = format!(r"\\.\pipe\{}", daemon.endpoint_name);
    loop {
        let mut options = ServerOptions::new();
        options.reject_remote_clients(true);
        let server = create_current_user_server(&options, &pipe_name)
            .map_err(|_| DaemonError::TransportUnavailable)?;
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| DaemonError::TransportUnavailable)?;
                if *shutdown.borrow() { return Ok(()); }
            }
            connected = server.connect() => {
                connected.map_err(|_| DaemonError::TransportUnavailable)?;
                let daemon = Arc::clone(&daemon);
                tokio::spawn(async move {
                    let mut server = server;
                    let _ = process_stream(&daemon, &mut server).await;
                });
            }
        }
    }
}

#[cfg(unix)]
async fn serve_platform(
    daemon: Arc<LocalDaemon>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!("{}.sock", daemon.endpoint_name));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|_| DaemonError::TransportUnavailable)?;
    }
    let listener =
        tokio::net::UnixListener::bind(&path).map_err(|_| DaemonError::TransportUnavailable)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| DaemonError::TransportUnavailable)?;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| DaemonError::TransportUnavailable)?;
                if *shutdown.borrow() { return Ok(()); }
            }
            accepted = listener.accept() => {
                let (mut stream, _) = accepted.map_err(|_| DaemonError::TransportUnavailable)?;
                let daemon = Arc::clone(&daemon);
                tokio::spawn(async move { let _ = process_stream(&daemon, &mut stream).await; });
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn serve_platform(
    _daemon: Arc<LocalDaemon>,
    _shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    Err(DaemonError::TransportUnavailable)
}

impl From<ApplicationError> for DaemonError {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::PolicyDenied(_) | ApplicationError::PermissionDenied => {
                Self::PermissionDenied
            }
            ApplicationError::Validation { .. } | ApplicationError::InvalidInferenceRequest => {
                Self::InvalidRequest
            }
            ApplicationError::NotFound { .. } => Self::NotFound,
            ApplicationError::Conflict { .. } => Self::Conflict,
            ApplicationError::InferenceUnavailable | ApplicationError::InferenceTimeout => {
                Self::TransportUnavailable
            }
            ApplicationError::RateLimited { .. } => Self::RateLimited,
            ApplicationError::AuthenticationFailed => Self::AuthenticationFailed,
            ApplicationError::QuotaExceeded => Self::QuotaExceeded,
            ApplicationError::ContextOverflow => Self::ContextOverflow,
            ApplicationError::SecretStoreUnavailable => Self::SecretStoreUnavailable,
            ApplicationError::Storage(_)
            | ApplicationError::NoSuitableModel
            | ApplicationError::MalformedModelOutput { .. }
            | ApplicationError::Internal => Self::StartupFailed,
        }
    }
}

impl DaemonError {
    const fn wire_code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::PermissionDenied => "permission_denied",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::InvalidConfiguration | Self::StartupFailed => "unavailable",
            Self::TransportUnavailable => "transport_unavailable",
            Self::SecretStoreUnavailable => "secret_store_unavailable",
            Self::RateLimited => "rate_limited",
            Self::AuthenticationFailed => "auth_failed",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ContextOverflow => "context_overflow",
            Self::InvalidInferenceRequest => "invalid_inference_request",
        }
    }
}

#[allow(dead_code)]
fn _assert_opaque_identity(_principal: PrincipalId, _operation: OperationId) {}

#[allow(dead_code)]
fn _assert_workspace_scope(_workspace: WorkspaceId, _capability: Capability) {}

#[allow(dead_code)]
fn _io_error_is_redacted(_: io::Error) -> DaemonError {
    DaemonError::TransportUnavailable
}

#[cfg(test)]
mod tests {
    use cortex_application::Capability;
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        DaemonConfig, DaemonRequest, LocalDaemon, PROTOCOL_VERSION, PairingChallenge,
        PairingResponse, ProvisionedLocalClient, WireResult, process_stream, read_frame,
        write_json_frame,
    };

    async fn request_over_wire(
        daemon: LocalDaemon,
        client: &ProvisionedLocalClient,
        request: DaemonRequest,
    ) -> super::DaemonResponse {
        let (mut wire_client, mut server) = tokio::io::duplex(128 * 1024);
        let serving = tokio::spawn(async move { process_stream(&daemon, &mut server).await });
        let challenge: PairingChallenge =
            serde_json::from_slice(&read_frame(&mut wire_client).await.expect("challenge"))
                .expect("challenge JSON");
        write_json_frame(&mut wire_client, &client.pairing_response(&challenge))
            .await
            .expect("proof");
        write_json_frame(&mut wire_client, &request)
            .await
            .expect("request");
        let response =
            serde_json::from_slice(&read_frame(&mut wire_client).await.expect("response"))
                .expect("response JSON");
        serving.await.expect("server task").expect("wire request");
        response
    }

    #[test]
    fn secret_store_unavailability_round_trips_across_ipc_error_mapping() {
        let daemon_error =
            super::DaemonError::from(cortex_application::ApplicationError::SecretStoreUnavailable);
        assert_eq!(daemon_error, super::DaemonError::SecretStoreUnavailable);
        assert_eq!(
            super::application_error_from_daemon(&daemon_error),
            cortex_application::ApplicationError::SecretStoreUnavailable
        );
    }

    #[tokio::test]
    async fn stream_requires_a_fresh_signed_challenge_before_dispatch() {
        let directory = TempDir::new().expect("temporary directory");
        let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
            .await
            .expect("daemon starts");
        let (mut client, mut server) = tokio::io::duplex(128 * 1024);
        let serving = tokio::spawn(async move { process_stream(&daemon, &mut server).await });

        let _challenge: PairingChallenge =
            serde_json::from_slice(&read_frame(&mut client).await.expect("challenge frame"))
                .expect("challenge JSON");
        let replay = PairingResponse {
            protocol_version: PROTOCOL_VERSION,
            signature: vec![0; 64],
        };
        write_json_frame(&mut client, &replay)
            .await
            .expect("proof frame");
        assert!(serving.await.expect("server task").is_err());
    }

    #[tokio::test]
    async fn signed_stream_proof_reaches_daemon_owned_context_not_client_principal() {
        let directory = TempDir::new().expect("temporary directory");
        let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
            .await
            .expect("daemon starts");
        let paired = daemon.paired_client();
        let (mut client, mut server) = tokio::io::duplex(128 * 1024);
        let serving = tokio::spawn(async move { process_stream(&daemon, &mut server).await });
        let challenge: PairingChallenge =
            serde_json::from_slice(&read_frame(&mut client).await.expect("challenge frame"))
                .expect("challenge JSON");
        write_json_frame(&mut client, &paired.pairing_response(&challenge))
            .await
            .expect("proof frame");
        let request_id = Uuid::now_v7();
        write_json_frame(
            &mut client,
            &DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_daemon_status".to_owned(),
                payload: json!({}),
            },
        )
        .await
        .expect("request frame");
        let response: super::DaemonResponse =
            serde_json::from_slice(&read_frame(&mut client).await.expect("response frame"))
                .expect("response JSON");
        assert!(matches!(response.result, WireResult::Success { .. }));
        assert_eq!(response.request_id, request_id);
        serving
            .await
            .expect("server task")
            .expect("authenticated stream");
    }

    #[tokio::test]
    async fn provisioned_client_authenticates_an_independent_daemon_after_restart() {
        let directory = TempDir::new().expect("temporary directory");
        let database_path = directory.path().join("cortex.db");
        let first_config = DaemonConfig::from_database_path(database_path.clone()).expect("config");
        let client = first_config.provisioned_client();
        let first = LocalDaemon::start(first_config)
            .await
            .expect("first daemon");
        drop(first);
        let second_config =
            DaemonConfig::from_database_path(database_path).expect("restart config");
        let daemon = LocalDaemon::start(second_config)
            .await
            .expect("second daemon");
        let (mut wire_client, mut server) = tokio::io::duplex(128 * 1024);
        let serving = tokio::spawn(async move { process_stream(&daemon, &mut server).await });
        let challenge: PairingChallenge =
            serde_json::from_slice(&read_frame(&mut wire_client).await.expect("challenge"))
                .expect("challenge JSON");
        write_json_frame(&mut wire_client, &client.pairing_response(&challenge))
            .await
            .expect("proof");
        write_json_frame(
            &mut wire_client,
            &DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: Uuid::now_v7(),
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_daemon_status".to_owned(),
                payload: json!({}),
            },
        )
        .await
        .expect("request");
        let response: super::DaemonResponse =
            serde_json::from_slice(&read_frame(&mut wire_client).await.expect("response"))
                .expect("response JSON");
        assert!(matches!(response.result, WireResult::Success { .. }));
        serving
            .await
            .expect("server task")
            .expect("authenticated stream");
    }

    #[tokio::test]
    async fn search_and_agent_wire_requests_are_policy_audited() {
        let directory = TempDir::new().expect("temporary directory");
        let config = DaemonConfig::for_test(directory.path());
        let client = config.provisioned_client();
        let daemon = LocalDaemon::start(config).await.expect("daemon");
        let search_id = Uuid::now_v7();
        let search = request_over_wire(
            daemon.clone(),
            &client,
            DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: search_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_knowledge_search".to_owned(),
                payload: json!({"query":"nothing"}),
            },
        )
        .await;
        assert!(matches!(search.result, WireResult::Success { .. }));
        assert_eq!(
            daemon
                .audit
                .count_for_correlation(daemon.workspace_id, search_id)
                .await
                .expect("audit"),
            1
        );
        let agent_id = Uuid::now_v7();
        let agent = request_over_wire(
            daemon.clone(),
            &client,
            DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: agent_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_agent_run".to_owned(),
                payload: json!({"prompt":"hello"}),
            },
        )
        .await;
        // SCRUM-84: transport-level inference failures carry the typed
        // transport code rather than the coarse startup-failure code.
        assert_eq!(
            agent.result,
            WireResult::Error {
                code: "transport_unavailable".to_owned()
            }
        );
        assert_eq!(
            daemon
                .audit
                .count_for_correlation(daemon.workspace_id, agent_id)
                .await
                .expect("audit"),
            1
        );
    }

    #[tokio::test]
    async fn denied_wire_search_returns_safe_error_and_writes_a_denial_audit() {
        let directory = TempDir::new().expect("temporary directory");
        let config = DaemonConfig::for_test(directory.path())
            .with_bootstrap_grants(vec![Capability::KnowledgeCreate]);
        let client = config.provisioned_client();
        let daemon = LocalDaemon::start(config).await.expect("daemon");
        let request_id = Uuid::now_v7();
        let response = request_over_wire(
            daemon.clone(),
            &client,
            DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_knowledge_search".to_owned(),
                payload: json!({"query":"private"}),
            },
        )
        .await;
        assert_eq!(
            response.result,
            WireResult::Error {
                code: "permission_denied".to_owned()
            }
        );
        assert_eq!(
            daemon
                .audit
                .count_for_correlation(daemon.workspace_id, request_id)
                .await
                .expect("audit"),
            1
        );
        let agent_id = Uuid::now_v7();
        let agent = request_over_wire(
            daemon.clone(),
            &client,
            DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: agent_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_agent_run".to_owned(),
                payload: json!({"prompt":"private"}),
            },
        )
        .await;
        assert_eq!(
            agent.result,
            WireResult::Error {
                code: "permission_denied".to_owned()
            }
        );
        assert_eq!(
            daemon
                .audit
                .count_for_correlation(daemon.workspace_id, agent_id)
                .await
                .expect("audit"),
            1
        );
    }

    #[tokio::test]
    async fn revoked_search_grant_remains_denied_and_audited_after_restart() {
        let directory = TempDir::new().expect("temporary directory");
        let database_path = directory.path().join("cortex.db");
        let first_config =
            DaemonConfig::from_database_path(database_path.clone()).expect("first config");
        let client = first_config.provisioned_client();
        let workspace_id = first_config.workspace_id;
        let principal_id = first_config.principal_id;
        let first = LocalDaemon::start(first_config)
            .await
            .expect("first daemon");
        first
            .database
            .repositories()
            .revoke_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
            .await
            .expect("revoke search grant");
        drop(first);

        let restarted = LocalDaemon::start(
            DaemonConfig::from_database_path(database_path).expect("restart config"),
        )
        .await
        .expect("restarted daemon");
        let request_id = Uuid::now_v7();
        let response = request_over_wire(
            restarted.clone(),
            &client,
            DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_knowledge_search".to_owned(),
                payload: json!({"query":"private"}),
            },
        )
        .await;
        assert_eq!(
            response.result,
            WireResult::Error {
                code: "permission_denied".to_owned()
            }
        );
        assert_eq!(
            restarted
                .audit
                .count_for_correlation(restarted.workspace_id, request_id)
                .await
                .expect("denial audit"),
            1
        );
    }

    #[tokio::test]
    async fn oversized_success_is_replaced_with_a_correlated_bounded_wire_error() {
        let request_id = Uuid::now_v7();
        let response = super::DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Success {
                value: json!({"value": "x".repeat(super::MAX_FRAME_BYTES)}),
            },
        };
        let (mut reader, mut writer) = tokio::io::duplex(128 * 1024);
        let write =
            tokio::spawn(async move { super::write_response(&mut writer, &response).await });
        let bytes = read_frame(&mut reader).await.expect("fallback frame");
        let emitted: super::DaemonResponse = serde_json::from_slice(&bytes).expect("fallback JSON");
        assert!(bytes.len() <= super::MAX_FRAME_BYTES);
        assert_eq!(emitted.request_id, request_id);
        assert_eq!(
            emitted.result,
            WireResult::Error {
                code: "response_too_large".to_owned()
            }
        );
        write.await.expect("writer task").expect("fallback write");
    }

    #[test]
    fn search_wire_response_preserves_bounded_source_citations() {
        let source = cortex_domain::EntityId::new();
        let response = super::fused_search_response(
            Uuid::now_v7(),
            vec![cortex_search::FusedHit {
                leg: cortex_search::FusedLeg::Memory(cortex_search::SearchHit {
                    entity_id: cortex_domain::EntityId::new(),
                    kind: cortex_application::EntityKind::Memory,
                    snippet: "cited statement".to_owned(),
                    lexical_rank: Some(1),
                    semantic_rank: None,
                    fused_score: 1.0,
                    sources: vec![cortex_domain::SourceRef { source_id: source }],
                    semantic_degraded: false,
                }),
                fused_score: 1.0,
            }],
        );
        let WireResult::Success { value } = response.result else {
            panic!("search response is successful");
        };
        assert_eq!(value[0]["sources"][0], Uuid::from(source).to_string());
    }
}
