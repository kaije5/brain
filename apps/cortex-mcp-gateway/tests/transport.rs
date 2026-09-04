use std::net::{IpAddr, Ipv4Addr};

use axum::{body::Body, http::Request};
use cortex_domain::PrincipalId;
use cortex_mcp::McpPrincipal as LocalMcpPrincipal;
use cortex_mcp_gateway::{
    GatewayConfig, GatewayTransport, OidcMetadata, PairedIdentityResolver, PrincipalRegistry,
    bind_loopback,
};
use cortexd::{DaemonConfig, LocalDaemon};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

pub mod support;

#[tokio::test]
async fn listener_is_bound_only_to_ipv4_loopback() {
    let config = GatewayConfig::parse(test_config()).expect("valid config");
    let listener = bind_loopback(&config).await.expect("loopback listener");
    assert_eq!(
        listener.local_addr().expect("local address").ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
}

#[tokio::test]
async fn missing_bearer_is_rejected_before_mcp_dispatch() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let principal_id = PrincipalId::new();
    let mut registry = PrincipalRegistry::new();
    registry.insert(
        principal_id,
        LocalMcpPrincipal::from_authenticated(daemon.paired_client()),
    );
    let resolver = PairedIdentityResolver::new(
        OidcMetadata::new("https://issuer.example", "cortex").expect("valid metadata"),
        Vec::new(),
        Vec::new(),
    )
    .expect("fail-closed resolver");
    let transport = GatewayTransport::new(resolver, registry, CancellationToken::new());
    let response = transport
        .router()
        .oneshot(
            Request::post("/mcp")
                .header("host", "localhost")
                .body(Body::from(valid_mcp_request()))
                .expect("valid request"),
        )
        .await
        .expect("infallible service");
    assert_eq!(response.status(), 401);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("bounded response")
        .to_bytes();
    assert!(!String::from_utf8_lossy(&bytes).contains("tools/list"));
}

#[tokio::test]
async fn valid_paired_bearer_injects_the_registered_daemon_principal() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let principal_id = PrincipalId::new();
    let mut registry = PrincipalRegistry::new();
    registry.insert(
        principal_id,
        LocalMcpPrincipal::from_authenticated(daemon.paired_client()),
    );
    let transport = GatewayTransport::new(
        support::resolver("paired-subject", principal_id),
        registry,
        CancellationToken::new(),
    );
    let token = support::encoded_token("paired-subject", "https://issuer.example", "cortex", 300);
    let response = transport
        .router()
        .oneshot(
            Request::post("/mcp")
                .header("host", "localhost")
                .header("authorization", format!("Bearer {token}"))
                .header("accept", "application/json, text/event-stream")
                .header("content-type", "application/json")
                .body(Body::from(valid_mcp_request()))
                .expect("valid request"),
        )
        .await
        .expect("infallible service");
    assert_eq!(response.status(), 200);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("bounded response")
        .to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("tools"));
    assert!(!body.contains(&token));
}

fn valid_mcp_request() -> &'static str {
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
}

fn test_config() -> &'static str {
    r#"{
      "local_port": 0,
      "oidc": {"issuer":"https://issuer.example","audience":"cortex","keys":[]},
      "paired_subjects": [],
      "relay": {"host":"relay.example","port":443,"server_name":"relay.example"}
    }"#
}
