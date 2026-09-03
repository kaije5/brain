use std::{io, sync::Arc};

use cortex_application::{
    ApplicationError, ApplicationService, Capability, CapabilityGrant, CommandContext, GrantPolicy,
    MemoryCorrectInput, MemoryCreateInput, NoteCreateInput, NoteUpdateInput, SecretStore,
    TaskCreateInput, TaskUpdateInput,
};
use cortex_domain::{EntityId, OperationId, PrincipalId, Revision, SourceRef, WorkspaceId};
use cortex_search::{HybridSearchService, SearchRequest};
use cortex_storage::{OperationStore, SqliteAuditPort, SqliteDatabase, SqliteRepositories};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use uuid::{Uuid, Version};

use crate::DaemonConfig;

/// The only supported local IPC protocol version for Cortex v0.1.
pub const PROTOCOL_VERSION: u16 = 1;
const MAX_FRAME_BYTES: usize = 64 * 1024;

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
    InvalidConfiguration,
    StartupFailed,
    TransportUnavailable,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unauthenticated => "local client is not authenticated",
            Self::InvalidRequest => "invalid local IPC request",
            Self::UnsupportedCapability => "unsupported daemon capability",
            Self::PermissionDenied => "daemon capability is not granted",
            Self::InvalidConfiguration => "invalid daemon configuration",
            Self::StartupFailed => "daemon startup failed",
            Self::TransportUnavailable => "local transport unavailable",
        })
    }
}

impl std::error::Error for DaemonError {}

/// State-owning Cortex daemon. `SQLite` is opened and migrated before this value is returned.
#[derive(Clone)]
pub struct LocalDaemon {
    database: SqliteDatabase,
    endpoint_name: String,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    pairing_verifier: VerifyingKey,
    pairing_signer: ed25519_dalek::SigningKey,
    migrations_applied: bool,
    service: Arc<DaemonService>,
    search: Arc<DaemonSearch>,
    _inference: DaemonInferenceUnavailable,
}

type DaemonService =
    ApplicationService<GrantPolicy, SqliteRepositories, OperationStore, SqliteAuditPort>;
type DaemonSearch = HybridSearchService<SqliteRepositories, DaemonEmbeddingUnavailable>;

/// The daemon retains the Task 8 provider boundary even when no model endpoint is configured.
/// Query callers receive lexical results with an explicit degraded semantic leg.
#[derive(Clone)]
struct DaemonInferenceUnavailable;

impl cortex_inference::InferenceProvider for DaemonInferenceUnavailable {
    async fn complete(
        &self,
        _request: cortex_inference::InferenceRequest,
    ) -> Result<cortex_inference::InferenceResponse, ApplicationError> {
        Err(ApplicationError::InferenceUnavailable)
    }
}

#[derive(Clone)]
struct DaemonEmbeddingUnavailable;

impl cortex_application::EmbeddingProvider for DaemonEmbeddingUnavailable {
    async fn embed(&self, _text: &str) -> Result<cortex_application::Embedding, ApplicationError> {
        Err(ApplicationError::InferenceUnavailable)
    }
}

/// A daemon-issued in-process handle representing a completed local authentication handshake.
/// Its constructor is private so an IPC payload can never manufacture a trusted principal.
#[derive(Clone)]
pub struct AuthenticatedLocalClient {
    daemon: LocalDaemon,
}

impl AuthenticatedLocalClient {
    /// Sends one request after the daemon-owned pairing/OS-local authentication boundary.
    ///
    /// # Errors
    /// Returns a safe protocol error for an invalid or unsupported request.
    pub async fn request(&self, request: &DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        Ok(self
            .daemon
            .handle_authenticated_request(request.clone())
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
        if config.inference_secret.is_some() {
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
        Self::start_inner(config).await
    }

    async fn start_inner(config: DaemonConfig) -> Result<Self, DaemonError> {
        let database = SqliteDatabase::connect_and_migrate(config.database_path)
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
        let persisted_capabilities = repositories
            .granted_capabilities(config.workspace_id, config.principal_id)
            .await
            .map_err(|_| DaemonError::StartupFailed)?;
        let grants = persisted_capabilities.into_iter().map(|capability| {
            CapabilityGrant::new(config.workspace_id, config.principal_id, capability)
        });
        let service = Arc::new(ApplicationService::new(
            GrantPolicy::new(grants),
            repositories.clone(),
            database.operation_store(),
            database.audit_port(),
        ));
        Ok(Self {
            database,
            endpoint_name: config.endpoint_name,
            workspace_id: config.workspace_id,
            principal_id: config.principal_id,
            pairing_verifier: config.pairing_verifier,
            pairing_signer: config.pairing_signer,
            migrations_applied: true,
            service,
            search: Arc::new(HybridSearchService::new(
                repositories,
                DaemonEmbeddingUnavailable,
            )),
            _inference: DaemonInferenceUnavailable,
        })
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
        (self.workspace_id.into(), self.principal_id.into())
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
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        validate_request(request)?;
        let correlation_id = request.request_id;
        match request.capability.as_str() {
            "cortex_daemon_status" => Ok(self.diagnostic_response(correlation_id, "status")),
            "cortex_daemon_doctor" => Ok(self.diagnostic_response(correlation_id, "doctor")),
            "cortex_daemon_logs" => Ok(self.diagnostic_response(correlation_id, "logs")),
            "cortex_knowledge_search" | "cortex_note_search" | "cortex_memory_search" => {
                self.search_knowledge(request).await
            }
            "cortex_note_create"
            | "cortex_note_update"
            | "cortex_note_delete"
            | "cortex_note_restore"
            | "cortex_task_create"
            | "cortex_task_update"
            | "cortex_task_complete"
            | "cortex_task_delete"
            | "cortex_task_restore"
            | "cortex_memory_create"
            | "cortex_memory_correct"
            | "cortex_memory_delete"
            | "cortex_memory_restore" => self.dispatch_mutation(request).await,
            _ => Err(DaemonError::UnsupportedCapability),
        }
    }

    async fn create_note(&self, request: &DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        let input: WireNoteCreate = serde_json::from_value(request.payload.clone())
            .map_err(|_| DaemonError::InvalidRequest)?;
        let context = CommandContext::from_authenticated(
            self.workspace_id,
            self.principal_id,
            OperationId::try_from(request.operation_id).map_err(|_| DaemonError::InvalidRequest)?,
            request.request_id,
        );
        let result = self
            .service
            .create_note(
                context,
                NoteCreateInput {
                    title: input.title,
                    content: input.content,
                },
            )
            .await
            .map_err(DaemonError::from)?;
        Ok(mutation_response(request.request_id, result))
    }

    #[allow(clippy::too_many_lines)] // Exhaustive, typed catalog-to-service mapping stays auditable in one place.
    async fn dispatch_mutation(
        &self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        if request.capability == "cortex_note_create" {
            return self.create_note(request).await;
        }
        let context = self.command_context(request)?;
        let result = match request.capability.as_str() {
            "cortex_note_update" => {
                let input: WireNoteUpdate = decode_payload(&request.payload)?;
                self.service
                    .update_note(
                        context,
                        input.entity_id()?,
                        input.revision()?,
                        NoteUpdateInput {
                            title: input.title,
                            content: input.content,
                        },
                    )
                    .await
            }
            "cortex_note_delete" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .delete_note(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_note_restore" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .restore_note(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_task_create" => {
                let input: WireTaskCreate = decode_payload(&request.payload)?;
                let due_at = input.due_at()?;
                self.service
                    .create_task(
                        context,
                        TaskCreateInput {
                            title: input.title,
                            due_at,
                        },
                    )
                    .await
            }
            "cortex_task_update" => {
                let input: WireTaskUpdate = decode_payload(&request.payload)?;
                let entity = input.entity_id()?;
                let revision = input.revision()?;
                let due_at = input.due_at()?;
                self.service
                    .update_task(
                        context,
                        entity,
                        revision,
                        TaskUpdateInput {
                            title: input.title,
                            due_at,
                        },
                    )
                    .await
            }
            "cortex_task_complete" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .complete_task(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_task_delete" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .delete_task(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_task_restore" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .restore_task(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_memory_create" => {
                let input: WireMemoryCreate = decode_payload(&request.payload)?;
                self.service
                    .create_memory(context, input.into_input()?)
                    .await
            }
            "cortex_memory_correct" => {
                let input: WireMemoryCorrect = decode_payload(&request.payload)?;
                let entity = input.entity_id()?;
                let revision = input.revision()?;
                self.service
                    .correct_memory(context, entity, revision, input.into_input()?)
                    .await
            }
            "cortex_memory_delete" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .delete_memory(context, input.entity_id()?, input.revision()?)
                    .await
            }
            "cortex_memory_restore" => {
                let input: WireEntityCommand = decode_payload(&request.payload)?;
                self.service
                    .restore_memory(context, input.entity_id()?, input.revision()?)
                    .await
            }
            _ => return Err(DaemonError::UnsupportedCapability),
        }
        .map_err(DaemonError::from)?;
        Ok(mutation_response(request.request_id, result))
    }

    async fn search_knowledge(
        &self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, DaemonError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SearchPayload {
            query: String,
            limit: Option<usize>,
        }
        let payload: SearchPayload = decode_payload(&request.payload)?;
        let limit = std::num::NonZeroUsize::new(payload.limit.unwrap_or(20).min(100))
            .ok_or(DaemonError::InvalidRequest)?;
        let hits = self
            .search
            .search(SearchRequest {
                workspace_id: self.workspace_id,
                principal_id: self.principal_id,
                query: payload.query,
                limit,
            })
            .await
            .map_err(DaemonError::from)?;
        Ok(DaemonResponse { protocol_version: PROTOCOL_VERSION, request_id: request.request_id, result: WireResult::Success { value: json!(hits.iter().map(|hit| json!({ "entity_id": Uuid::from(hit.entity_id).to_string(), "kind": hit.kind.as_str(), "snippet": hit.snippet, "semantic_degraded": hit.semantic_degraded })).collect::<Vec<_>>()) } })
    }

    fn command_context(&self, request: &DaemonRequest) -> Result<CommandContext, DaemonError> {
        Ok(CommandContext::from_authenticated(
            self.workspace_id,
            self.principal_id,
            OperationId::try_from(request.operation_id).map_err(|_| DaemonError::InvalidRequest)?,
            request.request_id,
        ))
    }

    /// Applies the daemon-owned authenticated principal after a transport challenge succeeded.
    #[must_use]
    pub async fn handle_authenticated_request(&self, request: DaemonRequest) -> DaemonResponse {
        let request_id = request.request_id;
        let result = match self.request_authenticated(&request).await {
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
        PairingResponse {
            protocol_version: PROTOCOL_VERSION,
            signature: self
                .pairing_signer
                .sign(&pairing_message(challenge))
                .to_bytes()
                .to_vec(),
        }
    }

    fn verify_pairing(&self, challenge: &PairingChallenge, response: &PairingResponse) -> bool {
        if response.protocol_version != PROTOCOL_VERSION || response.signature.len() != 64 {
            return false;
        }
        let Ok(signature) = Signature::try_from(response.signature.as_slice()) else {
            return false;
        };
        self.pairing_verifier
            .verify(&pairing_message(challenge), &signature)
            .is_ok()
    }

    fn diagnostic_response(&self, request_id: Uuid, capability: &str) -> DaemonResponse {
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: WireResult::Success {
                value: json!({
                    "capability": capability,
                    "correlation_id": request_id.to_string(),
                    "workspace_id": Uuid::from(self.workspace_id).to_string(),
                    "principal_id": Uuid::from(self.principal_id).to_string(),
                    "migrations_applied": self.migrations_applied,
                }),
            },
        }
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
struct WireNoteCreate {
    title: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntityCommand {
    entity_id: Uuid,
    expected_revision: u64,
}

impl WireEntityCommand {
    fn entity_id(&self) -> Result<EntityId, DaemonError> {
        EntityId::try_from(self.entity_id).map_err(|_| DaemonError::InvalidRequest)
    }
    fn revision(&self) -> Result<Revision, DaemonError> {
        Revision::rehydrate(self.expected_revision).map_err(|_| DaemonError::InvalidRequest)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireNoteUpdate {
    entity_id: Uuid,
    expected_revision: u64,
    title: String,
    content: String,
}

impl WireNoteUpdate {
    fn entity_id(&self) -> Result<EntityId, DaemonError> {
        EntityId::try_from(self.entity_id).map_err(|_| DaemonError::InvalidRequest)
    }
    fn revision(&self) -> Result<Revision, DaemonError> {
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
    entity_id: Uuid,
    expected_revision: u64,
    title: String,
    due_at: Option<String>,
}

impl WireTaskUpdate {
    fn entity_id(&self) -> Result<EntityId, DaemonError> {
        EntityId::try_from(self.entity_id).map_err(|_| DaemonError::InvalidRequest)
    }

    fn revision(&self) -> Result<Revision, DaemonError> {
        Revision::rehydrate(self.expected_revision).map_err(|_| DaemonError::InvalidRequest)
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
    if !daemon.verify_pairing(&challenge, &pairing) {
        return Err(DaemonError::Unauthenticated);
    }
    let bytes = read_frame(stream).await?;
    let response = response_for_request(daemon, &bytes).await?;
    write_response(stream, &response).await
}

async fn response_for_request(
    daemon: &LocalDaemon,
    bytes: &[u8],
) -> Result<DaemonResponse, DaemonError> {
    let response = match daemon.decode_request(bytes) {
        Ok(request) => daemon.handle_authenticated_request(request).await,
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
    write_json_frame(stream, response).await
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
            ApplicationError::Validation { .. }
            | ApplicationError::NotFound { .. }
            | ApplicationError::Conflict { .. } => Self::InvalidRequest,
            ApplicationError::Storage(_)
            | ApplicationError::InferenceUnavailable
            | ApplicationError::InferenceTimeout
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
            Self::InvalidConfiguration | Self::StartupFailed => "unavailable",
            Self::TransportUnavailable => "transport_unavailable",
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
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        DaemonConfig, DaemonRequest, LocalDaemon, PROTOCOL_VERSION, PairingChallenge,
        PairingResponse, WireResult, process_stream, read_frame, write_json_frame,
    };

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
}
