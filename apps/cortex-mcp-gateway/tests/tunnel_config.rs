use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cortex_mcp_gateway::{
    GatewayConfig, GatewayError, RelayEndpoint, RetryPolicy, TunnelClient, TunnelConnector,
};
use tokio_util::sync::CancellationToken;

#[test]
fn tunnel_configuration_has_no_public_listen_address() {
    let config = GatewayConfig::parse(test_config()).expect("valid config");
    assert!(config.public_listen_addr.is_none());
    assert_eq!(
        config.local_bind_addr(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    );
}

#[test]
fn public_listen_configuration_is_rejected() {
    let public = test_config().replace(
        "\"local_port\": 0,",
        "\"local_port\": 0, \"public_listen_addr\": \"0.0.0.0:8080\",",
    );
    assert!(matches!(
        GatewayConfig::parse(&public),
        Err(GatewayError::InvalidConfiguration)
    ));
}

#[test]
fn reconnect_delay_grows_exponentially_but_never_exceeds_the_cap() {
    let policy = RetryPolicy::new(Duration::from_millis(100), Duration::from_millis(500))
        .expect("valid retry policy");
    let actual: Vec<_> = (0..6).map(|attempt| policy.delay(attempt)).collect();
    assert_eq!(
        actual,
        vec![
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
            Duration::from_millis(500),
            Duration::from_millis(500),
            Duration::from_millis(500),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn tunnel_reconnects_until_cancellation_and_then_stops_cleanly() {
    let cancellation = CancellationToken::new();
    let connector = FailingConnector {
        attempts: Arc::new(AtomicUsize::new(0)),
        cancellation: cancellation.clone(),
    };
    let attempts = Arc::clone(&connector.attempts);
    let client = TunnelClient::new(
        RelayEndpoint::new("relay.example", 443, "relay.example").expect("valid relay"),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 33117),
        connector,
        RetryPolicy::new(Duration::from_millis(10), Duration::from_millis(40))
            .expect("valid policy"),
    )
    .expect("valid tunnel client");
    client.run(cancellation).await.expect("clean cancellation");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[derive(Clone)]
struct FailingConnector {
    attempts: Arc<AtomicUsize>,
    cancellation: CancellationToken,
}

impl TunnelConnector for FailingConnector {
    async fn connect_and_forward(
        &self,
        _relay: &RelayEndpoint,
        _local_addr: SocketAddr,
        _cancellation: CancellationToken,
    ) -> Result<(), GatewayError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            self.cancellation.cancel();
        }
        Err(GatewayError::TunnelUnavailable)
    }
}

fn test_config() -> &'static str {
    r#"{
      "local_port": 0,
      "oidc": {"issuer":"https://issuer.example","audience":"cortex","keys":[]},
      "paired_subjects": [],
      "relay": {"host":"relay.example","port":443,"server_name":"relay.example"}
    }"#
}
