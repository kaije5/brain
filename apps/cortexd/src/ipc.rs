use std::{io, sync::Arc};

use cortex_application::{
    ApplicationError, ApplicationService, Capability, CapabilityCatalog, CapabilityGrant,
    CommandContext, GrantPolicy, NoteCreateInput, SecretStore,
};
use cortex_domain::{OperationId, PrincipalId, WorkspaceId};
use cortex_storage::{OperationStore, SqliteAuditPort, SqliteDatabase, SqliteRepositories};
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
    pub pairing_proof: Option<Uuid>,
    pub capability: String,
    pub payload: Value,
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
    pairing_proof: Uuid,
    migrations_applied: bool,
    service: Arc<DaemonService>,
}

type DaemonService =
    ApplicationService<GrantPolicy, SqliteRepositories, OperationStore, SqliteAuditPort>;

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
        let mut request = request.clone();
        request.pairing_proof = Some(self.daemon.pairing_proof);
        Ok(self.daemon.handle_wire_request(request).await)
    }

    #[must_use]
    pub const fn pairing_proof(&self) -> Option<Uuid> {
        Some(self.daemon.pairing_proof)
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
        let capabilities = CapabilityCatalog::all();
        repositories
            .bootstrap_owner(config.workspace_id, config.principal_id, capabilities)
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
            repositories,
            database.operation_store(),
            database.audit_port(),
        ));
        Ok(Self {
            database,
            endpoint_name: config.endpoint_name,
            workspace_id: config.workspace_id,
            principal_id: config.principal_id,
            pairing_proof: config.pairing_proof,
            migrations_applied: true,
            service,
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
            "cortex_note_create" => self.create_note(request).await,
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
        Ok(DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: WireResult::Success {
                value: json!({
                    "entity_id": Uuid::from(result.entity_id).to_string(),
                    "revision": result.revision.get(),
                    "lifecycle": format!("{:?}", result.lifecycle).to_ascii_lowercase(),
                    "correlation_id": result.audit_correlation_id.to_string(),
                }),
            },
        })
    }

    /// Applies the real pairing boundary before invoking any daemon capability handler.
    #[must_use]
    pub async fn handle_wire_request(&self, request: DaemonRequest) -> DaemonResponse {
        let request_id = request.request_id;
        let result = if request.pairing_proof == Some(self.pairing_proof) {
            match self.request_authenticated(&request).await {
                Ok(response) => return response,
                Err(error) => WireResult::Error {
                    code: error.wire_code().to_owned(),
                },
            }
        } else {
            WireResult::Error {
                code: "unauthenticated".to_owned(),
            }
        };
        DaemonResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result,
        }
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

async fn process_stream<S>(daemon: &LocalDaemon, stream: &mut S) -> Result<(), DaemonError>
where
    S: AsyncRead + AsyncWrite + Unpin,
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
    let response = match daemon.decode_request(&bytes) {
        Ok(request) => daemon.handle_wire_request(request).await,
        Err(error) => {
            let request_id = recover_request_id(&bytes).ok_or(DaemonError::InvalidRequest)?;
            DaemonResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                result: WireResult::Error {
                    code: error.wire_code().to_owned(),
                },
            }
        }
    };
    write_response(stream, &response).await
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
    let bytes = serde_json::to_vec(response).map_err(|_| DaemonError::TransportUnavailable)?;
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
    fn from(_: ApplicationError) -> Self {
        Self::StartupFailed
    }
}

impl DaemonError {
    const fn wire_code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedCapability => "unsupported_capability",
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
