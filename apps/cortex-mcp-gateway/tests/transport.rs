use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
};

use axum::{body::Body, http::Request};
use cortex_domain::PrincipalId;
use cortex_mcp::McpPrincipal as LocalMcpPrincipal;
use cortex_mcp_gateway::{
    GatewayConfig, GatewayRateLimit, GatewayTransport, OidcMetadata, PairedIdentityResolver,
    PrincipalRegistry, bind_loopback,
};
use cortexd::{DaemonConfig, LocalDaemon};
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

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

#[tokio::test]
async fn authenticated_principal_rate_limit_rejects_burst_before_mcp_parsing_then_recovers() {
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
    let transport = GatewayTransport::new_with_rate_limit(
        support::resolver("paired-subject", principal_id),
        registry,
        CancellationToken::new(),
        GatewayRateLimit::new(2, Duration::from_secs(1)).expect("rate limit"),
    );
    let token = support::encoded_token("paired-subject", "https://issuer.example", "cortex", 300);

    for claimed_principal in ["attacker-one", "attacker-two"] {
        let response = transport
            .router()
            .oneshot(authenticated_request(
                &token,
                claimed_principal,
                valid_mcp_request(),
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), 200);
    }
    let rejected = transport
        .router()
        .oneshot(authenticated_request(&token, "attacker-three", "not-json"))
        .await
        .expect("rate rejection");
    assert_eq!(rejected.status(), 429);
    let bytes = http_body_util::BodyExt::collect(rejected.into_body())
        .await
        .expect("bounded response")
        .to_bytes();
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        r#"{"error":{"code":"cortex_rate_limited","message":"Request rate exceeded."}}"#
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    let recovered = transport
        .router()
        .oneshot(authenticated_request(
            &token,
            "attacker-four",
            valid_mcp_request(),
        ))
        .await
        .expect("recovered response");
    assert_eq!(recovered.status(), 200);
}

#[test]
fn duplicate_paired_principal_enrollments_are_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let enrollment_path = directory.path().join("remote-enrollment.json");
    let principal_id = Uuid::now_v7();
    fs::write(
        &enrollment_path,
        serde_json::to_vec(&serde_json::json!({
            "endpoint_name":"cortexd-test",
            "principal_id":principal_id,
            "signing_key":vec![7; 32]
        }))
        .expect("enrollment"),
    )
    .expect("write enrollment");
    let config = serde_json::json!({
        "local_port":0,
        "oidc":{
            "issuer":"https://issuer.example", "audience":"cortex",
            "algorithms":["EdDSA"], "cache_ttl_seconds":300
        },
        "paired_subjects":[
            {"subject":"remote-one", "principal_id":principal_id, "ipc_enrollment_path":enrollment_path},
            {"subject":"remote-two", "principal_id":principal_id, "ipc_enrollment_path":enrollment_path}
        ],
        "relay":{
            "host":"relay.example", "port":443, "server_name":"relay.example",
            "route_id":"route-1", "public_host":"cortex.example",
            "client_certificate_path":"client.pem", "client_private_key_path":"client.key"
        }
    });
    let config = GatewayConfig::parse(&config.to_string()).expect("configuration");
    assert!(matches!(
        config.paired_ipc_clients(),
        Err(cortex_mcp_gateway::GatewayError::InvalidConfiguration)
    ));
}

fn authenticated_request(
    token: &str,
    claimed_principal: &str,
    body: &'static str,
) -> Request<Body> {
    Request::post("/mcp")
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"))
        .header("x-cortex-principal", claimed_principal)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("valid request")
}

fn valid_mcp_request() -> &'static str {
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
}

fn test_config() -> &'static str {
    r#"{
      "local_port": 0,
      "oidc": {"issuer":"https://issuer.example","audience":"cortex","algorithms":["EdDSA"],"cache_ttl_seconds":300},
      "paired_subjects": [],
      "relay": {
        "host":"relay.example","port":443,"server_name":"relay.example",
        "route_id":"route-1","public_host":"cortex.example",
        "client_certificate_path":"client.pem","client_private_key_path":"client.key"
      }
    }"#
}
