#![allow(dead_code)] // Each integration-test binary imports only its scenario's shared harness API.

use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    routing::post,
};
use cortex_application::{
    AtomicMutation, AtomicMutationPort, Capability, CapabilityCatalog, CommandContext,
    MutationResult,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Lifecycle, OperationId, PolicyDecision,
    PrincipalId, Revision, Source, SourceInput, WorkspaceId,
};
use cortex_mcp::McpPrincipal;
use cortex_mcp_gateway::{
    GatewayTransport, OidcAlgorithm, OidcMetadata, OidcVerificationKey, PairedIdentityResolver,
    PairedSubject, PrincipalRegistry, RelayEndpoint, RetryPolicy, RustlsTunnelConnector,
    TunnelClient,
};
use cortex_storage::{RemoteEnrollmentRequest, SqliteDatabase};
use cortexd::{AuthenticatedIpcClient, DaemonConfig, DaemonRequest, PROTOCOL_VERSION};
use jsonwebtoken::{EncodingKey, Header, encode};
#[cfg(windows)]
use keyring_core::api::CredentialStoreApi;
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub fn configure_model_secret_for_platform(
    command: &mut Command,
    is_windows: bool,
    secret_reference: Option<&str>,
) {
    command.env_remove("CORTEX_MODEL_SECRET_REF");
    if is_windows && let Some(reference) = secret_reference {
        command.env("CORTEX_MODEL_SECRET_REF", reference);
    }
}

#[cfg(windows)]
pub async fn assert_deployed_daemon_degrades_on_missing_model_secret() {
    use cortexd::{AuthenticatedIpcClient, DaemonRequest, PROTOCOL_VERSION};

    let directory = TempDir::new().expect("temporary Cortex directory");
    let database_path = directory.path().join("cortex.db");
    let missing_reference = format!("keyring:cortex/e2e-missing-{}", Uuid::now_v7());
    std::fs::write(
        directory.path().join("cortexd.toml"),
        format!(
            "[models]
default_profile = \"e2e\"

[models.profiles.e2e]
base_url = \"http://127.0.0.1:9/\"
secret_ref = \"{missing_reference}\"
"
        ),
    )
    .expect("settings fixture");
    let mut process = Command::new(env!("CARGO_BIN_EXE_cortexd-e2e"))
        .env("CORTEX_DATABASE", database_path.clone())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("deployed cortexd process");

    // SCRUM-76: the daemon starts and serves IPC even though the model
    // secret can never resolve; inference must fail with an explicit
    // degraded error instead of taking the daemon down.
    let client = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(client) = AuthenticatedIpcClient::from_database_path(&database_path) {
                let request = DaemonRequest {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: Uuid::now_v7(),
                    principal_id: Uuid::now_v7(),
                    operation_id: Uuid::now_v7(),
                    capability: "cortex_daemon_status".to_owned(),
                    payload: serde_json::json!({}),
                };
                if client.request(&request).await.is_ok() {
                    return client;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("daemon became ready despite the missing model secret");

    let request = DaemonRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        operation_id: Uuid::now_v7(),
        capability: "cortex_agent_run".to_owned(),
        payload: serde_json::json!({"prompt": "hello"}),
    };
    let response = client
        .request(&request)
        .await
        .expect("agent run reaches the running daemon");
    match response.result {
        cortexd::WireResult::Error { code } => {
            // SCRUM-84: transport-level inference failures carry the typed
            // transport code rather than the coarse startup-failure code.
            assert_eq!(
                code, "transport_unavailable",
                "inference must fail explicitly"
            );
        }
        cortexd::WireResult::Success { .. } => {
            panic!("agent run succeeded without a resolvable model secret");
        }
    }
    let _ = process.kill().await;
    let _ = process.wait().await;
}

pub struct Harness {
    _directory: TempDir,
    #[cfg(windows)]
    _model_secret: PlatformSecretFixture,
    database_path: PathBuf,
    database: SqliteDatabase,
    workspace_id: WorkspaceId,
    remote_id: PrincipalId,
    source_id: EntityId,
    fake_model: FakeModelState,
    fake_model_cancellation: CancellationToken,
    fake_model_serving: JoinHandle<Result<(), std::io::Error>>,
    daemon_process: Child,
    gateway: RemoteGateway,
}

impl Harness {
    pub fn database_path(&self) -> &std::path::Path {
        &self.database_path
    }

    pub async fn start() -> Self {
        Self::start_with_remote_grants(CapabilityCatalog::all().to_vec()).await
    }

    pub async fn start_with_remote_grants(grants: Vec<Capability>) -> Self {
        Self::start_with_grants(CapabilityCatalog::all().to_vec(), grants).await
    }

    pub async fn start_with_owner_grants(grants: Vec<Capability>) -> Self {
        Self::start_with_grants(grants, CapabilityCatalog::all().to_vec()).await
    }

    async fn start_with_grants(
        owner_grants: Vec<Capability>,
        remote_grants: Vec<Capability>,
    ) -> Self {
        let directory = TempDir::new().expect("temporary Cortex directory");
        let database_path = directory.path().join("cortex.db");
        let fake_model = FakeModelState::new();
        let (model_base_url, fake_model_cancellation, fake_model_serving) =
            start_fake_model_endpoint(fake_model.clone()).await;
        #[cfg(windows)]
        let model_secret = PlatformSecretFixture::new();
        let owner_grant_fixture = owner_grants.clone();
        let mut config = DaemonConfig::from_database_path(database_path.clone())
            .expect("daemon config")
            .with_bootstrap_grants(owner_grants);
        let workspace_id = config.workspace_id();
        let owner_id = PrincipalId::try_from(config.owner_principal_id()).expect("owner principal");
        let bootstrap_database = SqliteDatabase::connect_and_migrate(&database_path)
            .await
            .expect("bootstrap database");
        bootstrap_database
            .repositories()
            .bootstrap_owner(workspace_id, owner_id, &owner_grant_fixture)
            .await
            .expect("owner grant fixture");
        let remote_id = PrincipalId::new();
        let record = bootstrap_database
            .operation_store()
            .enroll_remote_once(RemoteEnrollmentRequest {
                workspace_id,
                owner_principal_id: owner_id,
                operation_id: OperationId::new(),
                correlation_id: Uuid::now_v7(),
                subject: format!("e2e-remote-{}", Uuid::now_v7()),
                principal_id: remote_id,
                grants: remote_grants,
                pairing_verifier: config
                    .derived_remote_signing_key(remote_id)
                    .verifying_key()
                    .to_bytes(),
                max_remote_clients: 16,
            })
            .await
            .expect("durable remote enrollment");
        let enrollment_path = config
            .reconcile_remote_enrollment(&record)
            .expect("remote artifact reconciliation")
            .enrollment_path;
        drop(config);
        drop(bootstrap_database);
        #[cfg(windows)]
        let model_secret_reference = Some(model_secret.reference());
        #[cfg(not(windows))]
        let model_secret_reference: Option<String> = None;
        let mut settings = format!(
            "[brain]
prompt = \"E2E global Brain instructions are active.\"

[models]
default_profile = \"e2e\"

[models.profiles.e2e]
base_url = \"{model_base_url}\"
"
        );
        if let Some(reference) = model_secret_reference {
            use std::fmt::Write as _;
            let _ = writeln!(settings, "secret_ref = \"{reference}\"");
        }
        std::fs::write(directory.path().join("cortexd.toml"), settings).expect("settings fixture");
        let mut daemon_command = Command::new(env!("CARGO_BIN_EXE_cortexd-e2e"));
        daemon_command
            .env("CORTEX_DATABASE", &database_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let daemon_process = daemon_command.spawn().expect("deployed cortexd process");
        let owner =
            AuthenticatedIpcClient::from_database_path(&database_path).expect("owner enrollment");
        let remote = AuthenticatedIpcClient::from_enrollment_path(&enrollment_path)
            .expect("remote enrollment");
        wait_until_ready(&owner).await;
        let database = SqliteDatabase::connect_and_migrate(&database_path)
            .await
            .expect("test inspection database");
        let source_id = seed_source(&database, workspace_id, owner_id).await;
        *fake_model.source_id.lock().await = Some(source_id);
        let gateway = RemoteGateway::start(remote_id, remote.clone()).await;
        Self {
            _directory: directory,
            #[cfg(windows)]
            _model_secret: model_secret,
            database_path,
            database,
            workspace_id,
            remote_id,
            source_id,
            fake_model,
            fake_model_cancellation,
            fake_model_serving,
            daemon_process,
            gateway,
        }
    }

    #[must_use]
    pub const fn source_id(&self) -> EntityId {
        self.source_id
    }

    /// An authenticated owner IPC client: the surface TUI and other local
    /// IPC clients share. Created per call; enrollment is durable.
    pub fn ipc_client(&self) -> cortexd::AuthenticatedIpcClient {
        cortexd::AuthenticatedIpcClient::from_database_path(&self.database_path)
            .expect("owner IPC enrollment")
    }

    /// Sends one capability over the IPC surface as the paired owner.
    pub async fn ipc_call(&self, capability: &str, payload: Value) -> CallResult {
        let request = cortexd::DaemonRequest {
            protocol_version: cortexd::PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: capability.to_owned(),
            payload,
        };
        let response = self
            .ipc_client()
            .request(&request)
            .await
            .expect("IPC transport");
        CallResult::from_daemon(response)
    }

    pub async fn ipc_task_list(&self) -> CallResult {
        self.ipc_call("cortex_task_list", json!({"limit": 50}))
            .await
    }

    pub async fn ipc_task_complete(&self, resource_id: &str, revision: &str) -> CallResult {
        self.ipc_call(
            "cortex_task_complete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
    }

    pub async fn ipc_note_update(
        &self,
        resource_id: &str,
        revision: &str,
        title: &str,
        content: &str,
    ) -> CallResult {
        self.ipc_call(
            "cortex_note_update",
            json!({
                "resource_id": resource_id,
                "expected_revision": revision,
                "title": title,
                "content": content
            }),
        )
        .await
    }

    pub async fn ipc_note_delete(&self, resource_id: &str, revision: &str) -> CallResult {
        self.ipc_call(
            "cortex_note_delete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
    }

    pub async fn cli_remember(&self, statement: &str) -> CallResult {
        let source = Uuid::from(self.source_id).to_string();
        self.cli(&[
            "brain",
            "remember",
            statement,
            "--subject",
            "cortex",
            "--predicate",
            "uses",
            "--object",
            "nemotron",
            "--source",
            &source,
        ])
        .await
    }

    pub async fn cli_memory_search(&self, query: &str) -> CallResult {
        self.cli(&["brain", "memory", "search", query]).await
    }

    pub async fn assert_cli_text_search(&self, query: &str, expected: &str) {
        let output = Command::new(env!("CARGO_BIN_EXE_brain-e2e"))
            .args(["memory", "search", query])
            .env("CORTEX_DATABASE", &self.database_path)
            .output()
            .await
            .expect("real brain text output");
        assert!(output.status.success(), "brain exit: {:?}", output.status);
        let stdout = String::from_utf8(output.stdout).expect("brain text UTF-8");
        assert!(stdout.contains(expected), "brain text output: {stdout}");
    }

    /// Runs the real Brain CLI binary with JSON output.
    pub async fn cli(&self, arguments: &[&str]) -> CallResult {
        let mut process = Command::new(env!("CARGO_BIN_EXE_brain-e2e"));
        process
            .args(arguments.iter().skip(1))
            .arg("--output")
            .arg("json")
            .env("CORTEX_DATABASE", &self.database_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = process.output().await.expect("real brain process output");
        CallResult::from_cli_output(&output)
    }

    pub async fn mcp_remember(&self, statement: &str) -> CallResult {
        self.mcp_call(
            "memory.create",
            json!({
                "statement": statement,
                "normalized_subject": "cortex",
                "normalized_predicate": "uses",
                "normalized_object": "nemotron",
                "sources": [{"source_id": Uuid::from(self.source_id)}]
            }),
        )
        .await
    }

    pub async fn mcp_search(&self, query: &str) -> SearchResult {
        let value = self
            .gateway
            .call_paired("memory.search", json!({"query": query}))
            .await
            .assert_success_value();
        SearchResult(value)
    }

    pub async fn mcp_task_create(&self, title: &str) -> CallResult {
        self.mcp_call("task.create", json!({"title": title})).await
    }

    pub async fn mcp_task_list(&self) -> CallResult {
        self.mcp_call("task.list", json!({"limit": 50})).await
    }

    pub async fn mcp_task_complete(&self, resource_id: &str, revision: &str) -> CallResult {
        self.mcp_call(
            "task.complete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
    }

    pub async fn mcp_knowledge_update(
        &self,
        resource_id: &str,
        revision: &str,
        title: &str,
        content: &str,
    ) -> CallResult {
        self.mcp_call(
            "knowledge.update",
            json!({
                "resource_id": resource_id,
                "expected_revision": revision,
                "title": title,
                "content": content
            }),
        )
        .await
    }

    pub async fn mcp_knowledge_delete(&self, resource_id: &str, revision: &str) -> CallResult {
        self.mcp_call(
            "knowledge.delete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
    }

    pub async fn mcp_delete(&self, entity_id: EntityId, revision: u64) -> CallResult {
        self.mcp_call(
            "memory.delete",
            json!({"entity_id": Uuid::from(entity_id), "expected_revision": revision}),
        )
        .await
    }

    pub async fn mcp_restore(&self, entity_id: EntityId, revision: u64) -> CallResult {
        self.mcp_call(
            "memory.restore",
            json!({"entity_id": Uuid::from(entity_id), "expected_revision": revision}),
        )
        .await
    }

    async fn mcp_call(&self, name: &str, arguments: Value) -> CallResult {
        let response = self.gateway.call_paired(name, arguments).await;
        CallResult::from_gateway(&response)
    }

    pub async fn assert_unpaired_denied(&self) {
        let response = self
            .gateway
            .call_unpaired("memory.search", json!({"query": "must not dispatch"}))
            .await;
        assert_eq!(response.status, 401);
        assert_eq!(response.body["error"]["code"], "cortex_unauthorized");
    }

    pub async fn assert_delete_denial_audited(&self, denial: &CallResult) {
        let audit = self
            .database
            .audit_port()
            .find_for_correlation(self.workspace_id, denial.correlation_id())
            .await
            .expect("audit lookup")
            .expect("denial audit");
        assert_eq!(audit.principal_id, self.remote_id);
        assert_eq!(
            audit.capability,
            Capability::MemoryDelete.metadata().mcp_name
        );
        assert_eq!(
            audit.policy_decision,
            cortex_domain::PolicyDecision::Deny(cortex_domain::PolicyDeny::MissingGrant)
        );
        assert_eq!(audit.result, AuditResult::Rejected);
    }

    pub async fn assert_successful_audit(
        &self,
        correlation_id: Uuid,
        capability: &'static str,
        entity_id: EntityId,
    ) {
        let audit = self
            .database
            .audit_port()
            .find_for_correlation(self.workspace_id, correlation_id)
            .await
            .expect("audit lookup")
            .expect("mutation audit");
        assert_eq!(audit.capability, capability);
        assert_eq!(
            audit.target,
            Some(cortex_domain::ResourceTarget::CortexEntity(entity_id))
        );
        assert_eq!(audit.policy_decision, PolicyDecision::Allow);
        assert_eq!(audit.result, AuditResult::Succeeded);
    }

    pub async fn agent_remember(&self, statement: &str) -> CallResult {
        *self.fake_model.statement.lock().await = statement.to_owned();
        self.cli(&["brain", "ask", "Remember the supplied statement."])
            .await
    }

    /// The system message the fake model most recently received.
    pub async fn last_system_message(&self) -> Option<String> {
        self.fake_model.last_system_message.lock().await.clone()
    }

    pub async fn cli_agent_tools(&self) -> Vec<String> {
        self.cli(&["brain", "ask", "List available tools without using them."])
            .await
            .assert_success();
        self.fake_model.offered_tools.lock().await.clone()
    }

    pub async fn stop_fake_model(&self) {
        self.fake_model.available.store(false, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }

    pub async fn shutdown(mut self) {
        self.gateway.shutdown().await;
        self.daemon_process
            .kill()
            .await
            .expect("stop cortexd process");
        self.daemon_process
            .wait()
            .await
            .expect("join cortexd process");
        self.fake_model_cancellation.cancel();
        self.fake_model_serving
            .await
            .expect("fake model join")
            .expect("fake model clean shutdown");
    }
}

#[cfg(windows)]
struct PlatformSecretFixture {
    target: String,
}

#[cfg(windows)]
impl PlatformSecretFixture {
    fn new() -> Self {
        let target = format!("keyring:cortex/e2e-model-{}", Uuid::now_v7());
        let username = target
            .strip_prefix("keyring:cortex/")
            .expect("fixture keyring target");
        windows_native_keyring_store::Store::new()
            .and_then(|store| store.build("cortex", username, None))
            .and_then(|entry| entry.set_secret(b"fixture-not-a-live-secret"))
            .expect("create platform secret-store fixture");
        Self { target }
    }

    fn reference(&self) -> &str {
        &self.target
    }
}

#[cfg(windows)]
impl Drop for PlatformSecretFixture {
    fn drop(&mut self) {
        if let Some(username) = self.target.strip_prefix("keyring:cortex/") {
            let _ = windows_native_keyring_store::Store::new()
                .and_then(|store| store.build("cortex", username, None))
                .and_then(|entry| entry.delete_credential());
        }
    }
}

async fn wait_until_ready(client: &AuthenticatedIpcClient) {
    for _ in 0..100 {
        let request = DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_daemon_status".to_owned(),
            payload: json!({}),
        };
        if client.request(&request).await.is_ok() {
            // SCRUM-76: IPC is available before background model resolution
            // finishes; chat-capable tests must also wait for the catalog.
            let models_request = DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: Uuid::now_v7(),
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_model_list".to_owned(),
                payload: json!({}),
            };
            if let Ok(response) = client.request(&models_request).await
                && let cortexd::WireResult::Success { value } = response.result
                && value["models"]
                    .as_array()
                    .is_some_and(|models| !models.is_empty())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not become ready");
}

async fn seed_source(
    database: &SqliteDatabase,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
) -> EntityId {
    let source = Source::create(SourceInput {
        workspace_id,
        reference: "explicit-user://task-13".to_owned(),
    })
    .expect("source fixture");
    let correlation_id = Uuid::now_v7();
    let context = CommandContext::from_authenticated(
        workspace_id,
        principal_id,
        OperationId::new(),
        correlation_id,
    );
    let result = MutationResult {
        entity_id: source.id(),
        revision: Revision::initial(),
        lifecycle: Lifecycle::Active,
        audit_correlation_id: correlation_id,
    };
    let audit = AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id: context.operation_id,
        correlation_id,
        capability: Capability::MemoryCreate.metadata().mcp_name,
        target: Some(cortex_domain::ResourceTarget::CortexEntity(source.id())),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    };
    let mutation = AtomicMutation::new(
        context,
        Capability::MemoryCreate,
        None,
        vec![cortex_application::AggregateChange::InsertSource(
            source.clone(),
        )],
        result,
        audit,
    )
    .expect("source seed mutation");
    database
        .operation_store()
        .execute_once(mutation)
        .await
        .expect("source seed persisted");
    source.id()
}

#[derive(Clone)]
struct FakeModelState {
    available: Arc<AtomicBool>,
    statement: Arc<Mutex<String>>,
    source_id: Arc<Mutex<Option<EntityId>>>,
    offered_tools: Arc<Mutex<Vec<String>>>,
    last_system_message: Arc<Mutex<Option<String>>>,
}

impl FakeModelState {
    fn new() -> Self {
        Self {
            available: Arc::new(AtomicBool::new(true)),
            statement: Arc::new(Mutex::new(String::new())),
            source_id: Arc::new(Mutex::new(None)),
            offered_tools: Arc::new(Mutex::new(Vec::new())),
            last_system_message: Arc::new(Mutex::new(None)),
        }
    }
}

async fn start_fake_model_endpoint(
    state: FakeModelState,
) -> (
    String,
    CancellationToken,
    JoinHandle<Result<(), std::io::Error>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake model listener");
    let address = listener.local_addr().expect("fake model address");
    let cancellation = CancellationToken::new();
    let service = Router::new()
        .route("/v1/models", get(fake_models))
        .route("/models", get(fake_models))
        .route("/v1/embeddings", post(fake_embedding))
        .route("/v1/chat/completions", post(fake_chat))
        .route("/embeddings", post(fake_embedding))
        .route("/chat/completions", post(fake_chat))
        .with_state(state);
    let serving_cancellation = cancellation.clone();
    let serving = tokio::spawn(async move {
        axum::serve(listener, service)
            .with_graceful_shutdown(serving_cancellation.cancelled_owned())
            .await
    });
    (format!("http://{address}/"), cancellation, serving)
}

async fn fake_models(State(state): State<FakeModelState>) -> Response {
    if !state.available.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({"data": [{"id": "nemotron-test"}]})).into_response()
}

async fn fake_embedding(State(state): State<FakeModelState>) -> Response {
    if !state.available.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({"data": [{"embedding": [1.0, 0.0, 0.0]}]})).into_response()
}

async fn fake_chat(State(state): State<FakeModelState>, Json(body): Json<Value>) -> Response {
    if !state.available.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    // Discovery capability probes are single "ping" turns carrying the
    // cortex_probe tool; answer with evidence the probes accept instead of
    // running the agent-mutation script below.
    let is_probe = body["messages"].as_array().is_some_and(|messages| {
        messages.len() == 1 && messages[0]["role"] == "user" && messages[0]["content"] == "ping"
    }) && body["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["function"]["name"] == "cortex_probe")
    });
    if is_probe {
        return Json(json!({
            "choices": [{"message": {
                "content": "{}",
                "tool_calls": [{
                    "id": "probe-1",
                    "function": {"name": "cortex_probe", "arguments": "{}"}
                }]
            }}]
        }))
        .into_response();
    }
    let has_tool_result = body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"));
    if let Some(system) = body["messages"]
        .as_array()
        .and_then(|messages| messages.first())
        .filter(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
    {
        *state.last_system_message.lock().await = Some(system.to_owned());
    }
    *state.offered_tools.lock().await = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect();
    let message = if has_tool_result {
        json!({"content": "Remembered.", "tool_calls": []})
    } else {
        let statement = state.statement.lock().await.clone();
        if statement.is_empty() {
            return Json(json!({
                "choices": [{"message": {"content": "No mutation requested.", "tool_calls": []}}]
            }))
            .into_response();
        }
        let source_id = *state.source_id.lock().await;
        let Some(source_id) = source_id else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let arguments = json!({
            "statement": statement,
            "normalized_subject": "cortex",
            "normalized_predicate": "stores",
            "normalized_object": "canonical local state",
            "sources": [{"source_id": Uuid::from(source_id)}]
        });
        json!({
            "content": null,
            "tool_calls": [{
                "id": "remember-1",
                "function": {
                    "name": "cortex_memory_create",
                    "arguments": arguments.to_string()
                }
            }]
        })
    };
    Json(json!({"choices": [{"message": message}]})).into_response()
}

const TEST_PRIVATE_KEY: &[u8] = br"-----BEGIN PRIVATE KEY-----
MHICAQEwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC
oB8wHQYKKoZIhvcNAQkJFDEPDA1DdXJkbGUgQ2hhaXJzgSEAGb9ECWmEzf6FQbrB
Z9w7lshQhqowtrbLDFw4rXAxZuE=
-----END PRIVATE KEY-----
";
const TEST_PUBLIC_KEY: &[u8] = br"-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=
-----END PUBLIC KEY-----
";

#[derive(Serialize)]
struct TestClaims<'a> {
    iss: &'a str,
    aud: &'a str,
    sub: &'a str,
    exp: u64,
}

struct RelayCommand {
    bearer: String,
    body: Value,
    response: oneshot::Sender<GatewayResponse>,
}

struct GatewayResponse {
    status: u16,
    body: Value,
}

impl GatewayResponse {
    fn assert_success_value(self) -> Value {
        assert_eq!(self.status, 200, "gateway response: {}", self.body);
        assert_ne!(self.body["result"]["isError"], true, "{:?}", self.body);
        self.body["result"]["structuredContent"].clone()
    }
}

struct RemoteGateway {
    commands: mpsc::Sender<RelayCommand>,
    paired_token: String,
    unpaired_token: String,
    next_request: AtomicU64,
    cancellation: CancellationToken,
    relay_task: JoinHandle<()>,
    gateway_task: JoinHandle<Result<(), cortex_mcp_gateway::GatewayError>>,
    tunnel_task: JoinHandle<Result<(), cortex_mcp_gateway::GatewayError>>,
}

impl RemoteGateway {
    #[allow(clippy::too_many_lines)] // Keeps one auditable deployed gateway/tunnel composition fixture.
    async fn start(remote_id: PrincipalId, remote: AuthenticatedIpcClient) -> Self {
        let cancellation = CancellationToken::new();
        let gateway_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("gateway loopback listener");
        let gateway_address = gateway_listener.local_addr().expect("gateway address");
        let mut principals = PrincipalRegistry::new();
        principals.insert(remote_id, McpPrincipal::from_ipc(remote));
        let resolver = PairedIdentityResolver::new(
            OidcMetadata::new("https://issuer.example", "cortex").expect("OIDC metadata"),
            vec![
                OidcVerificationKey::from_pem("test-key", OidcAlgorithm::EdDsa, TEST_PUBLIC_KEY)
                    .expect("OIDC verification key"),
            ],
            vec![PairedSubject::new("paired-subject", remote_id).expect("paired subject")],
        )
        .expect("paired resolver");
        let gateway = GatewayTransport::new(resolver, principals, cancellation.clone());
        let gateway_cancellation = cancellation.clone();
        let gateway_task = tokio::spawn(gateway.serve(gateway_listener, gateway_cancellation));

        let relay_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake relay listener");
        let relay_address = relay_listener.local_addr().expect("relay address");
        let (commands, mut command_receiver) = mpsc::channel::<RelayCommand>(8);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let relay_cancellation = cancellation.clone();
        let relay_task = tokio::spawn(async move {
            let (stream, _) = relay_listener.accept().await.expect("outbound tunnel");
            let mut tls = test_relay_acceptor()
                .accept(stream)
                .await
                .expect("mutually authenticated tunnel");
            let registration = read_tunnel_frame(&mut tls).await;
            assert_eq!(registration["type"], "register");
            write_tunnel_frame(&mut tls, &json!({"type":"registered","version":1})).await;
            ready_sender.send(()).expect("relay ready");
            while let Some(command) = command_receiver.recv().await {
                write_tunnel_frame(
                    &mut tls,
                    &json!({
                        "type":"request",
                        "version":1,
                        "request_id":Uuid::now_v7(),
                        "method":"POST",
                        "path":"/mcp",
                        "headers":{
                            "authorization":format!("Bearer {}", command.bearer),
                            "accept":"application/json, text/event-stream",
                            "content-type":"application/json"
                        },
                        "body":command.body.to_string()
                    }),
                )
                .await;
                let response = read_tunnel_frame(&mut tls).await;
                let body = response["body"].as_str().expect("gateway response body");
                command
                    .response
                    .send(GatewayResponse {
                        status: u16::try_from(response["status"].as_u64().expect("gateway status"))
                            .expect("bounded gateway status"),
                        body: parse_mcp_body(body),
                    })
                    .ok();
            }
            relay_cancellation.cancel();
        });
        let connector = RustlsTunnelConnector::with_client_identity(
            include_bytes!("../../../../apps/cortex-mcp-gateway/tests/fixtures/ca.pem"),
            include_bytes!("../../../../apps/cortex-mcp-gateway/tests/fixtures/client.pem"),
            include_bytes!("../../../../apps/cortex-mcp-gateway/tests/fixtures/client.key"),
        )
        .expect("tunnel client identity");
        let endpoint = RelayEndpoint::new(
            relay_address.ip().to_string(),
            relay_address.port(),
            "relay.test",
            "task-13",
            "cortex.example",
        )
        .expect("relay endpoint");
        let tunnel = TunnelClient::new(
            endpoint,
            gateway_address,
            connector,
            RetryPolicy::new(Duration::from_millis(5), Duration::from_millis(20))
                .expect("retry policy"),
        )
        .expect("outbound tunnel");
        let tunnel_cancellation = cancellation.clone();
        let tunnel_task = tokio::spawn(tunnel.run(tunnel_cancellation));
        tokio::time::timeout(Duration::from_secs(5), ready_receiver)
            .await
            .expect("tunnel registration timeout")
            .expect("tunnel registration");
        Self {
            commands,
            paired_token: test_token("paired-subject"),
            unpaired_token: test_token("unpaired-subject"),
            next_request: AtomicU64::new(1),
            cancellation,
            relay_task,
            gateway_task,
            tunnel_task,
        }
    }

    async fn call_paired(&self, name: &str, arguments: Value) -> GatewayResponse {
        self.call(&self.paired_token, name, arguments).await
    }

    async fn call_unpaired(&self, name: &str, arguments: Value) -> GatewayResponse {
        self.call(&self.unpaired_token, name, arguments).await
    }

    async fn call(&self, bearer: &str, name: &str, arguments: Value) -> GatewayResponse {
        let id = self.next_request.fetch_add(1, Ordering::SeqCst);
        let (response_sender, response_receiver) = oneshot::channel();
        self.commands
            .send(RelayCommand {
                bearer: bearer.to_owned(),
                body: json!({
                    "jsonrpc":"2.0",
                    "id":id,
                    "method":"tools/call",
                    "params":{"name":name,"arguments":arguments}
                }),
                response: response_sender,
            })
            .await
            .expect("relay request");
        tokio::time::timeout(Duration::from_secs(10), response_receiver)
            .await
            .expect("relay response timeout")
            .expect("relay response")
    }

    async fn shutdown(self) {
        drop(self.commands);
        self.cancellation.cancel();
        self.relay_task.await.expect("relay task");
        self.gateway_task
            .await
            .expect("gateway task")
            .expect("gateway shutdown");
        self.tunnel_task
            .await
            .expect("tunnel task")
            .expect("tunnel shutdown");
    }
}

fn test_relay_acceptor() -> TlsAcceptor {
    let root = CertificateDer::from_pem_slice(include_bytes!(
        "../../../../apps/cortex-mcp-gateway/tests/fixtures/ca.pem"
    ))
    .expect("relay CA");
    let mut roots = RootCertStore::empty();
    roots.add(root).expect("client root");
    let verifier = WebPkiClientVerifier::builder(roots.into())
        .build()
        .expect("client verifier");
    let certificate = CertificateDer::from_pem_slice(include_bytes!(
        "../../../../apps/cortex-mcp-gateway/tests/fixtures/server.pem"
    ))
    .expect("relay certificate");
    let private_key = PrivateKeyDer::from_pem_slice(include_bytes!(
        "../../../../apps/cortex-mcp-gateway/tests/fixtures/server.key"
    ))
    .expect("relay private key");
    TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![certificate], private_key)
            .expect("relay TLS configuration"),
    ))
}

async fn read_tunnel_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Value {
    let length = stream.read_u32_le().await.expect("tunnel frame length");
    assert!(length <= 64 * 1024);
    let mut bytes = vec![0; usize::try_from(length).expect("frame length")];
    stream
        .read_exact(&mut bytes)
        .await
        .expect("tunnel frame body");
    serde_json::from_slice(&bytes).expect("tunnel frame JSON")
}

async fn write_tunnel_frame<S: AsyncWrite + Unpin>(stream: &mut S, value: &Value) {
    let bytes = serde_json::to_vec(value).expect("tunnel frame JSON");
    stream
        .write_u32_le(u32::try_from(bytes.len()).expect("bounded frame"))
        .await
        .expect("tunnel frame length");
    stream.write_all(&bytes).await.expect("tunnel frame body");
    stream.flush().await.expect("tunnel flush");
}

fn parse_mcp_body(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| {
        body.lines()
            .find_map(|line| line.strip_prefix("data: "))
            .and_then(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| json!({"raw":body}))
    })
}

fn test_token(subject: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let mut header = Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.kid = Some("test-key".to_owned());
    encode(
        &header,
        &TestClaims {
            iss: "https://issuer.example",
            aud: "cortex",
            sub: subject,
            exp: now.saturating_add(300),
        },
        &EncodingKey::from_ed_pem(TEST_PRIVATE_KEY).expect("test signing key"),
    )
    .expect("test bearer")
}

pub struct CallResult {
    value: Option<Value>,
    error: Option<String>,
    correlation_id: Uuid,
}

impl CallResult {
    /// The successful result data, when the call succeeded.
    #[must_use]
    pub fn data(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    /// The wire error code, when the call failed.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn success(value: Value, correlation_id: Uuid) -> Self {
        Self {
            value: Some(value),
            error: None,
            correlation_id,
        }
    }

    fn error(code: &str, correlation_id: Uuid) -> Self {
        Self {
            value: None,
            error: Some(code.to_owned()),
            correlation_id,
        }
    }

    fn from_cli_output(output: &std::process::Output) -> Self {
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "brain emitted invalid JSON: stdout={:?}, stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        if output.status.success() {
            assert_eq!(envelope["ok"], true, "{envelope}");
            let value = envelope["data"].clone();
            let correlation_id = correlation_id(&value).unwrap_or_else(Uuid::now_v7);
            Self::success(value, correlation_id)
        } else {
            assert_eq!(
                envelope["ok"],
                false,
                "stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self::error(
                envelope["error"]["code"].as_str().expect("CLI error code"),
                Uuid::now_v7(),
            )
        }
    }

    fn from_daemon(response: cortexd::DaemonResponse) -> Self {
        match response.result {
            cortexd::WireResult::Success { value } => {
                let correlation_id = correlation_id(&value).unwrap_or_else(Uuid::now_v7);
                Self::success(value, correlation_id)
            }
            cortexd::WireResult::Error { code } => {
                let correlation = Uuid::now_v7();
                Self::error(&code, correlation)
            }
        }
    }

    fn from_gateway(response: &GatewayResponse) -> Self {
        assert_eq!(response.status, 200, "gateway response: {}", response.body);
        let structured = &response.body["result"]["structuredContent"];
        if response.body["result"]["isError"] == true {
            let correlation_id = correlation_id(structured).expect("MCP denial correlation");
            return Self::error(
                structured["code"].as_str().expect("MCP error code"),
                correlation_id,
            );
        }
        let value = structured.clone();
        let correlation_id = correlation_id(&value).unwrap_or_else(Uuid::now_v7);
        Self::success(value, correlation_id)
    }

    pub fn assert_success(&self) {
        assert!(self.error.is_none(), "unexpected error: {:?}", self.error);
        assert!(self.value.is_some());
    }

    pub fn assert_permission_denied(&self) {
        assert_eq!(self.error.as_deref(), Some("cortex_permission_denied"));
    }

    pub fn assert_lifecycle(&self, lifecycle: &str) {
        self.assert_success();
        assert_eq!(self.value()["lifecycle"], lifecycle);
    }

    pub fn assert_contains(&self, entity_id: EntityId) {
        self.assert_success();
        assert!(self.values().iter().any(|value| {
            value["entity_id"].as_str() == Some(Uuid::from(entity_id).to_string().as_str())
        }));
    }

    pub fn assert_not_contains(&self, entity_id: EntityId) {
        self.assert_success();
        assert!(!self.values().iter().any(|value| {
            value["entity_id"].as_str() == Some(Uuid::from(entity_id).to_string().as_str())
        }));
    }

    pub fn assert_first_statement(&self, expected: &str) {
        self.assert_success();
        assert_eq!(
            self.values().first().and_then(|v| v["snippet"].as_str()),
            Some(expected)
        );
    }

    pub fn assert_first_semantic_degraded(&self, expected: bool) {
        self.assert_success();
        assert_eq!(
            self.values()
                .first()
                .and_then(|value| value["semantic_degraded"].as_bool()),
            Some(expected)
        );
    }

    #[must_use]
    pub fn entity_id(&self) -> EntityId {
        let id = self.value()["entity_id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .expect("mutation entity UUID");
        EntityId::try_from(id).expect("mutation entity UUIDv7")
    }

    #[must_use]
    pub const fn correlation_id(&self) -> Uuid {
        self.correlation_id
    }

    fn value(&self) -> &Value {
        self.value.as_ref().expect("successful value")
    }

    fn values(&self) -> &[Value] {
        self.value().as_array().expect("search result array")
    }
}

fn correlation_id(value: &Value) -> Option<Uuid> {
    value
        .get("correlation_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

pub struct SearchResult(Value);

impl SearchResult {
    #[must_use]
    pub fn first_statement(&self) -> &str {
        self.0[0]["snippet"].as_str().expect("first snippet")
    }

    #[must_use]
    pub fn first_sources(&self) -> Vec<EntityId> {
        self.0[0]["sources"]
            .as_array()
            .expect("source array")
            .iter()
            .map(|value| {
                let id = value
                    .as_str()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .expect("source UUID");
                EntityId::try_from(id).expect("source UUIDv7")
            })
            .collect()
    }

    #[must_use]
    pub fn first_semantic_degraded(&self) -> bool {
        self.0[0]["semantic_degraded"]
            .as_bool()
            .expect("semantic degradation flag")
    }
}
