use cortex_mcp::{HttpSecurityConfig, McpError, McpPrincipal, McpServer};
use cortexd::{DaemonConfig, LocalDaemon};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn tool_invocation_uses_authenticated_principal_not_input_workspace() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let server = McpServer::new(McpPrincipal::from_authenticated(daemon.paired_client()));

    let result = server
        .call_tool("cortex_knowledge_search", json!({"query": "Cortex"}))
        .await
        .expect("paired request is dispatched");
    assert!(result.is_array());

    let rejected = server
        .call_tool(
            "cortex_knowledge_search",
            json!({"query": "Cortex", "workspace_id": "attacker"}),
        )
        .await;
    assert!(matches!(rejected, Err(McpError { code, .. }) if code == "cortex_invalid_input"));
}

#[test]
fn loopback_origin_allowlist_does_not_accept_lookalike_hosts() {
    let policy = HttpSecurityConfig::loopback(CancellationToken::new());
    assert!(policy.accepts("localhost", Some("http://localhost"), 1));
    assert!(!policy.accepts("localhost", Some("http://localhost.attacker"), 1));
}
