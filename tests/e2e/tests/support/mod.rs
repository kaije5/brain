#![allow(dead_code)] // Each integration-test binary imports only its scenario's shared harness API.

use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use brain::{Cli, command_request};
use clap::Parser;
use cortex_application::{
    ApplicationError, AtomicMutation, AtomicMutationPort, Capability, CapabilityCatalog,
    CommandContext, MutationResult, SecretRef, SecretStore,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Lifecycle, OperationId, PolicyDecision,
    PrincipalId, Revision, Source, SourceInput, WorkspaceId,
};
use cortex_inference::{OpenAiCompatibleConfig, ProviderLimits};
use cortex_mcp::{McpError, McpPrincipal, McpServer};
use cortex_mcp_gateway::{GatewayError, RelayEndpoint, RetryPolicy, TunnelClient, TunnelConnector};
use cortex_storage::SqliteDatabase;
use cortexd::{
    AuthenticatedIpcClient, DaemonConfig, DaemonError, DaemonRequest, LocalDaemon,
    PROTOCOL_VERSION, WireResult,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub struct Harness {
    _directory: TempDir,
    database: SqliteDatabase,
    daemon: Arc<LocalDaemon>,
    owner: AuthenticatedIpcClient,
    remote: AuthenticatedIpcClient,
    unpaired: AuthenticatedIpcClient,
    remote_mcp: McpPrincipal,
    source_id: EntityId,
    fake_model: FakeModelState,
    fake_model_cancellation: CancellationToken,
    fake_model_serving: JoinHandle<Result<(), std::io::Error>>,
    shutdown_sender: watch::Sender<bool>,
    serving: JoinHandle<Result<(), DaemonError>>,
}

impl Harness {
    pub async fn start() -> Self {
        Self::start_with_remote_grants(CapabilityCatalog::all().to_vec()).await
    }

    pub async fn start_with_remote_grants(grants: Vec<Capability>) -> Self {
        let directory = TempDir::new().expect("temporary Cortex directory");
        let database_path = directory.path().join("cortex.db");
        let fake_model = FakeModelState::new();
        let (model_base_url, fake_model_cancellation, fake_model_serving) =
            start_fake_model_endpoint(fake_model.clone()).await;
        let mut config = DaemonConfig::from_database_path(database_path.clone())
            .expect("daemon config")
            .with_inference_secret(
                SecretRef::new("test-secret-store://task-13-model")
                    .expect("fake model secret reference"),
            )
            .with_model_config(model_config(&model_base_url));
        let remote_id = PrincipalId::new();
        let enrollment_path = config
            .enroll_remote_principal(remote_id, &grants)
            .expect("remote MCP enrollment");
        let unpaired_path = directory.path().join("unpaired-enrollment.json");
        let mut unpaired_enrollment: Value =
            serde_json::from_slice(&fs::read(&enrollment_path).expect("paired enrollment fixture"))
                .expect("paired enrollment JSON");
        unpaired_enrollment["signing_key"] = json!(vec![42_u8; 32]);
        fs::write(
            &unpaired_path,
            serde_json::to_vec(&unpaired_enrollment).expect("unpaired enrollment JSON"),
        )
        .expect("unpaired enrollment fixture");
        let daemon = Arc::new(
            LocalDaemon::start_with_secret_store(config, &FakeSecretStore)
                .await
                .expect("daemon starts"),
        );
        let (shutdown_sender, shutdown) = watch::channel(false);
        let serving = tokio::spawn(Arc::clone(&daemon).serve(shutdown));
        let owner =
            AuthenticatedIpcClient::from_database_path(&database_path).expect("owner enrollment");
        let remote = AuthenticatedIpcClient::from_enrollment_path(&enrollment_path)
            .expect("remote enrollment");
        let unpaired = AuthenticatedIpcClient::from_enrollment_path(&unpaired_path)
            .expect("unpaired client fixture");
        wait_until_ready(&owner).await;
        let database = SqliteDatabase::connect_and_migrate(&database_path)
            .await
            .expect("test inspection database");
        let source_id = seed_source(&database, &daemon).await;
        *fake_model.source_id.lock().await = Some(source_id);
        let remote_mcp = McpPrincipal::from_ipc(remote.clone());
        exercise_in_memory_tunnel().await;
        Self {
            _directory: directory,
            database,
            daemon,
            owner,
            remote,
            unpaired,
            remote_mcp,
            source_id,
            fake_model,
            fake_model_cancellation,
            fake_model_serving,
            shutdown_sender,
            serving,
        }
    }

    #[must_use]
    pub const fn source_id(&self) -> EntityId {
        self.source_id
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
        self.cli(&["brain", "memory", "search", query, "--output", "json"])
            .await
    }

    async fn cli(&self, arguments: &[&str]) -> CallResult {
        let cli = Cli::try_parse_from(arguments).expect("valid CLI acceptance command");
        let command = command_request(&cli).expect("valid daemon command");
        let correlation_id = command.request_id;
        let request = command.into_daemon_request(Uuid::from(self.owner.principal_id()));
        let response = self
            .owner
            .request(&request)
            .await
            .expect("CLI IPC response");
        CallResult::from_wire(response.result, correlation_id)
    }

    pub async fn mcp_remember(&self, statement: &str) -> CallResult {
        self.mcp_call(
            "cortex_memory_create",
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
        let value = McpServer::new()
            .call_tool_as(
                &self.remote_mcp,
                "cortex_memory_search",
                json!({"query": query}),
            )
            .await
            .expect("paired MCP search");
        SearchResult(value)
    }

    pub async fn mcp_delete(&self, entity_id: EntityId, revision: u64) -> CallResult {
        self.mcp_call(
            "cortex_memory_delete",
            json!({"entity_id": Uuid::from(entity_id), "expected_revision": revision}),
        )
        .await
    }

    pub async fn mcp_restore(&self, entity_id: EntityId, revision: u64) -> CallResult {
        self.mcp_call(
            "cortex_memory_restore",
            json!({"entity_id": Uuid::from(entity_id), "expected_revision": revision}),
        )
        .await
    }

    async fn mcp_call(&self, name: &str, arguments: Value) -> CallResult {
        match McpServer::new()
            .call_tool_as(&self.remote_mcp, name, arguments)
            .await
        {
            Ok(value) => {
                let correlation_id = value
                    .get("correlation_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .unwrap_or_else(Uuid::now_v7);
                CallResult::success(value, correlation_id)
            }
            Err(error) => CallResult::mcp_error(&error),
        }
    }

    pub async fn assert_unpaired_denied(&self) {
        let correlation_id = Uuid::now_v7();
        let request = DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: correlation_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_memory_search".to_owned(),
            payload: json!({"query": "must not dispatch"}),
        };
        assert!(matches!(
            self.unpaired.request(&request).await,
            Err(DaemonError::Unauthenticated | DaemonError::TransportUnavailable)
        ));
        assert_eq!(
            self.database
                .audit_port()
                .count_for_correlation(self.daemon.ownership_workspace_id(), correlation_id)
                .await
                .expect("unpaired audit count"),
            0
        );
    }

    pub async fn assert_delete_denial_audited(&self, denial: &CallResult) {
        let audit = self
            .database
            .audit_port()
            .find_for_correlation(
                self.daemon.ownership_workspace_id(),
                denial.correlation_id(),
            )
            .await
            .expect("audit lookup")
            .expect("denial audit");
        assert_eq!(audit.principal_id, self.remote.principal_id());
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
            .find_for_correlation(self.daemon.ownership_workspace_id(), correlation_id)
            .await
            .expect("audit lookup")
            .expect("mutation audit");
        assert_eq!(audit.capability, capability);
        assert_eq!(audit.target_id, Some(entity_id));
        assert_eq!(audit.policy_decision, PolicyDecision::Allow);
        assert_eq!(audit.result, AuditResult::Succeeded);
    }

    pub async fn agent_remember(&self, statement: &str) -> CallResult {
        *self.fake_model.statement.lock().await = statement.to_owned();
        self.cli(&["brain", "ask", "Remember the supplied statement."])
            .await
    }

    pub async fn remote_agent_tools(&self) -> Vec<String> {
        let response = self
            .remote
            .request(&DaemonRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: Uuid::now_v7(),
                principal_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_agent_run".to_owned(),
                payload: json!({"prompt": "List available tools without using them."}),
            })
            .await
            .expect("paired agent response");
        assert!(matches!(response.result, WireResult::Success { .. }));
        self.fake_model.offered_tools.lock().await.clone()
    }

    pub async fn stop_fake_model(&self) {
        self.fake_model.available.store(false, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }

    pub async fn shutdown(self) {
        self.shutdown_sender.send(true).expect("daemon shutdown");
        self.serving
            .await
            .expect("daemon join")
            .expect("daemon clean shutdown");
        self.fake_model_cancellation.cancel();
        self.fake_model_serving
            .await
            .expect("fake model join")
            .expect("fake model clean shutdown");
    }
}

struct FakeSecretStore;

impl SecretStore for FakeSecretStore {
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretRef, ApplicationError> {
        SecretRef::new(reference.as_str())
    }
}

async fn wait_until_ready(client: &AuthenticatedIpcClient) {
    for _ in 0..50 {
        let request = DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_daemon_status".to_owned(),
            payload: json!({}),
        };
        if client.request(&request).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("daemon did not become ready");
}

async fn seed_source(database: &SqliteDatabase, daemon: &LocalDaemon) -> EntityId {
    let (workspace_id, principal_id) = daemon.ownership_identity();
    let workspace_id = WorkspaceId::try_from(workspace_id).expect("workspace id");
    let principal_id = PrincipalId::try_from(principal_id).expect("principal id");
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
        target_id: Some(source.id()),
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
}

impl FakeModelState {
    fn new() -> Self {
        Self {
            available: Arc::new(AtomicBool::new(true)),
            statement: Arc::new(Mutex::new(String::new())),
            source_id: Arc::new(Mutex::new(None)),
            offered_tools: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn model_config(base_url: &str) -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig::new(
        base_url,
        "nemotron-test",
        None,
        Duration::from_secs(1),
        ProviderLimits::new(64 * 1024, 32 * 1024, 32).expect("provider limits"),
    )
    .expect("local model config")
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
        .route("/v1/embeddings", post(fake_embedding))
        .route("/v1/chat/completions", post(fake_chat))
        .with_state(state);
    let serving_cancellation = cancellation.clone();
    let serving = tokio::spawn(async move {
        axum::serve(listener, service)
            .with_graceful_shutdown(serving_cancellation.cancelled_owned())
            .await
    });
    (format!("http://{address}/v1/"), cancellation, serving)
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
    let has_tool_result = body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"));
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

#[derive(Clone)]
struct InMemoryTunnel {
    called: Arc<AtomicBool>,
}

impl TunnelConnector for InMemoryTunnel {
    async fn connect_and_forward(
        &self,
        _relay: &RelayEndpoint,
        local_addr: SocketAddr,
        cancellation: CancellationToken,
    ) -> Result<(), GatewayError> {
        assert!(local_addr.ip().is_loopback());
        self.called.store(true, Ordering::SeqCst);
        cancellation.cancel();
        Ok(())
    }
}

async fn exercise_in_memory_tunnel() {
    let called = Arc::new(AtomicBool::new(false));
    let cancellation = CancellationToken::new();
    let tunnel = TunnelClient::new(
        RelayEndpoint::new(
            "relay.example",
            443,
            "relay.example",
            "task-13",
            "cortex.example",
        )
        .expect("relay fixture"),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 33117),
        InMemoryTunnel {
            called: Arc::clone(&called),
        },
        RetryPolicy::new(Duration::from_millis(1), Duration::from_millis(1))
            .expect("retry fixture"),
    )
    .expect("in-memory tunnel fixture");
    tunnel.run(cancellation).await.expect("tunnel cancellation");
    assert!(called.load(Ordering::SeqCst));
}

pub struct CallResult {
    value: Option<Value>,
    error: Option<String>,
    correlation_id: Uuid,
}

impl CallResult {
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

    fn mcp_error(error: &McpError) -> Self {
        Self::error(
            &error.code,
            error
                .correlation_id
                .expect("dispatched MCP error correlation"),
        )
    }

    fn from_wire(result: WireResult, correlation_id: Uuid) -> Self {
        match result {
            WireResult::Success { value } => Self::success(value, correlation_id),
            WireResult::Error { code } => Self::error(&code, correlation_id),
        }
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
