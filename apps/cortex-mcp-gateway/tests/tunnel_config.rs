use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cortex_mcp::McpPrincipal as LocalMcpPrincipal;
use cortex_mcp_gateway::{
    GatewayConfig, GatewayError, GatewayTransport, PrincipalRegistry, RelayEndpoint, RetryPolicy,
    RustlsTunnelConnector, TunnelClient, TunnelConnector,
};
use cortexd::{DaemonConfig, LocalDaemon};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub mod support;

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
        RelayEndpoint::new(
            "relay.example",
            443,
            "relay.example",
            "route-1",
            "cortex.example",
        )
        .expect("valid relay"),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 33117),
        connector,
        RetryPolicy::new(Duration::from_millis(10), Duration::from_millis(40))
            .expect("valid policy"),
    )
    .expect("valid tunnel client");
    client.run(cancellation).await.expect("clean cancellation");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mutual_tls_relay_registration_forwards_one_bounded_request_to_loopback_mcp() {
    let directory = TempDir::new().expect("temporary directory");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let principal_id = cortex_domain::PrincipalId::new();
    let mut principals = PrincipalRegistry::new();
    principals.insert(
        principal_id,
        LocalMcpPrincipal::from_authenticated(daemon.paired_client()),
    );
    let cancellation = CancellationToken::new();
    let gateway_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("gateway listener");
    let gateway_addr = gateway_listener.local_addr().expect("gateway address");
    let gateway = GatewayTransport::new(
        support::resolver("paired-subject", principal_id),
        principals,
        cancellation.clone(),
    );
    let gateway_task = tokio::spawn(gateway.serve(gateway_listener, cancellation.clone()));

    let relay_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("relay listener");
    let relay_addr = relay_listener.local_addr().expect("relay address");
    let relay_acceptor = test_relay_acceptor();
    let relay_cancellation = cancellation.clone();
    let relay_task = tokio::spawn(async move {
        let (tcp, _) = relay_listener.accept().await.expect("outbound connection");
        let mut tls = relay_acceptor
            .accept(tcp)
            .await
            .expect("authenticated client TLS");
        let registration = read_frame(&mut tls).await;
        assert_eq!(registration["type"], "register");
        assert_eq!(registration["route_id"], "route-1");
        assert_eq!(registration["public_host"], "cortex.example");
        write_frame(&mut tls, &json!({"type":"registered","version":1})).await;

        let bearer =
            support::encoded_token("paired-subject", "https://issuer.example", "cortex", 300);
        write_frame(
            &mut tls,
            &json!({
                "type":"request", "version":1, "request_id":Uuid::now_v7(),
                "method":"POST", "path":"/mcp",
                "headers":{
                    "authorization":format!("Bearer {bearer}"),
                    "accept":"application/json, text/event-stream",
                    "content-type":"application/json"
                },
                "body":r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#
            }),
        )
        .await;
        let response = read_frame(&mut tls).await;
        assert_eq!(response["type"], "response");
        assert_eq!(response["status"], 200);
        assert!(
            response["body"]
                .as_str()
                .is_some_and(|body| body.contains("tools"))
        );
        relay_cancellation.cancel();
    });

    let connector = RustlsTunnelConnector::with_client_identity(
        include_bytes!("fixtures/ca.pem"),
        include_bytes!("fixtures/client.pem"),
        include_bytes!("fixtures/client.key"),
    )
    .expect("client TLS identity");
    let endpoint = RelayEndpoint::new(
        relay_addr.ip().to_string(),
        relay_addr.port(),
        "relay.test",
        "route-1",
        "cortex.example",
    )
    .expect("relay endpoint");
    let tunnel_result = connector
        .connect_and_forward(&endpoint, gateway_addr, cancellation.clone())
        .await;
    relay_task.await.expect("relay task");
    tunnel_result.expect("registered forwarding session");
    cancellation.cancel();
    gateway_task
        .await
        .expect("gateway task")
        .expect("gateway shutdown");
}

fn test_relay_acceptor() -> TlsAcceptor {
    let ca = CertificateDer::from_pem_slice(include_bytes!("fixtures/ca.pem")).expect("CA");
    let mut roots = RootCertStore::empty();
    roots.add(ca).expect("client root");
    let verifier = WebPkiClientVerifier::builder(roots.into())
        .build()
        .expect("verifier");
    let cert =
        CertificateDer::from_pem_slice(include_bytes!("fixtures/server.pem")).expect("server cert");
    let key =
        PrivateKeyDer::from_pem_slice(include_bytes!("fixtures/server.key")).expect("server key");
    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert], key)
        .expect("server config");
    TlsAcceptor::from(Arc::new(config))
}

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Value {
    let length = stream.read_u32_le().await.expect("frame length");
    assert!(length <= 64 * 1024);
    let mut bytes = vec![0; usize::try_from(length).expect("length")];
    stream.read_exact(&mut bytes).await.expect("frame body");
    serde_json::from_slice(&bytes).expect("JSON frame")
}

async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, value: &Value) {
    let bytes = serde_json::to_vec(value).expect("JSON frame");
    stream
        .write_u32_le(u32::try_from(bytes.len()).expect("bounded frame"))
        .await
        .expect("frame length");
    stream.write_all(&bytes).await.expect("frame body");
    stream.flush().await.expect("flush");
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
      "oidc": {"issuer":"https://issuer.example","audience":"cortex","algorithms":["EdDSA"],"cache_ttl_seconds":300},
      "paired_subjects": [],
      "relay": {
        "host":"relay.example","port":443,"server_name":"relay.example",
        "route_id":"route-1","public_host":"cortex.example",
        "client_certificate_path":"client.pem","client_private_key_path":"client.key"
      }
    }"#
}
