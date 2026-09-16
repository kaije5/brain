use bytes::Bytes;
use cortex_mcp::{HttpSecurityConfig, McpError, McpPrincipal, McpServer, streamable_http_service};
use cortexd::{AuthenticatedIpcClient, DaemonConfig, LocalDaemon};
use http::{Request, header};
use http_body_util::{BodyExt, Full};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower_service::Service;

#[tokio::test]
async fn file_enrolled_gateway_principal_dispatches_over_real_local_ipc() {
    let directory = TempDir::new().expect("temporary directory");
    let database_path = directory.path().join("cortex.db");
    let config = DaemonConfig::from_database_path(database_path.clone()).expect("config");
    let daemon = std::sync::Arc::new(LocalDaemon::start(config).await.expect("daemon starts"));
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&daemon).serve(shutdown));
    tokio::task::yield_now().await;
    let client = AuthenticatedIpcClient::from_database_path(&database_path).expect("enrollment");
    let principal = McpPrincipal::from_ipc(client);

    let result = McpServer::new()
        .call_tool_as(&principal, "knowledge.retrieve", json!({"query":"Cortex"}))
        .await
        .expect("paired IPC dispatch");
    assert!(result.is_array());

    shutdown_sender.send(true).expect("shutdown sent");
    serving.await.expect("server task").expect("clean shutdown");
}

const MCP_ACCEPT: &str = "application/json, text/event-stream";

#[tokio::test]
async fn tool_invocation_uses_authenticated_principal_not_input_workspace() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let principal = McpPrincipal::from_authenticated(daemon.paired_client());
    let server = McpServer::new();

    let result = server
        .call_tool_as(&principal, "knowledge.retrieve", json!({"query": "Cortex"}))
        .await
        .expect("paired request is dispatched");
    assert!(result.is_array());

    let rejected = server
        .call_tool_as(
            &principal,
            "knowledge.retrieve",
            json!({"query": "Cortex", "workspace_id": "attacker"}),
        )
        .await;
    assert!(matches!(rejected, Err(McpError { code, .. }) if code == "cortex_invalid_input"));
}

#[tokio::test]
async fn bounded_limit_is_rejected_before_daemon_authorization() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(
        DaemonConfig::for_test(directory.path()).with_bootstrap_grants(Vec::new()),
    )
    .await
    .expect("daemon starts");
    let principal = McpPrincipal::from_authenticated(daemon.paired_client());
    let result = McpServer::new()
        .call_tool_as(
            &principal,
            "knowledge.retrieve",
            json!({"query":"Cortex","limit":101}),
        )
        .await;
    assert!(matches!(result, Err(McpError { code, .. }) if code == "cortex_invalid_input"));
}

#[tokio::test]
async fn http_tool_dispatch_requires_gateway_injected_principal_context() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let principal = McpPrincipal::from_authenticated(daemon.paired_client());
    let mut service =
        streamable_http_service(&HttpSecurityConfig::loopback(CancellationToken::new()));

    let without_principal = service
        .call(tool_request(&json!({"query":"Cortex"})))
        .await
        .expect("service is infallible");
    let without_principal = response_text(without_principal).await;
    assert!(without_principal.contains("cortex_permission_denied"));

    let mut injected = tool_request(&json!({"query":"Cortex"}));
    injected.extensions_mut().insert(principal);
    let injected = service.call(injected).await.expect("service is infallible");
    let injected = response_text(injected).await;
    assert!(!injected.contains("cortex_permission_denied"));
    assert!(injected.contains("\"result\""));

    let mut forged = tool_request(&json!({"query":"Cortex","principal_id":"attacker"}));
    forged
        .extensions_mut()
        .insert(McpPrincipal::from_authenticated(daemon.paired_client()));
    let forged = service.call(forged).await.expect("service is infallible");
    let forged = response_text(forged).await;
    assert!(forged.contains("cortex_invalid_input"));
}

fn tool_request(arguments: &serde_json::Value) -> Request<Full<Bytes>> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "knowledge.retrieve",
            "arguments": arguments,
        }
    });
    Request::post("http://localhost/mcp")
        .header(header::HOST, "localhost")
        .header(header::ACCEPT, MCP_ACCEPT)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("test request is valid")
}

async fn response_text<B>(response: http::Response<B>) -> String
where
    B: http_body::Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response can be collected")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("response is UTF-8")
}
