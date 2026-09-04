use std::{collections::HashMap, fmt, net::SocketAddr, sync::Arc, time::Duration};

use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::{sleep, timeout},
};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::GatewayError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TUNNEL_FRAME_BYTES: usize = 64 * 1024;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;
const TUNNEL_PROTOCOL_VERSION: u16 = 1;

/// Vendor-neutral destination for the stateless relay.
#[derive(Clone, Eq, PartialEq)]
pub struct RelayEndpoint {
    host: String,
    port: u16,
    server_name: String,
    route_id: String,
    public_host: String,
}

impl RelayEndpoint {
    /// Creates a validated TLS relay endpoint without URL or credential syntax.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for an invalid host, server name, or port.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        server_name: impl Into<String>,
        route_id: impl Into<String>,
        public_host: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let host = host.into();
        let server_name = server_name.into();
        let route_id = route_id.into();
        let public_host = public_host.into();
        if !valid_host(&host)
            || !valid_host(&server_name)
            || !valid_identifier(&route_id)
            || !valid_host(&public_host)
            || port == 0
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        ServerName::try_from(server_name.clone())
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self {
            host,
            port,
            server_name,
            route_id,
            public_host,
        })
    }
}

impl fmt::Debug for RelayEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayEndpoint")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("server_name", &self.server_name)
            .field("route_id", &"[REDACTED]")
            .field("public_host", &self.public_host)
            .finish()
    }
}

/// Bounded exponential reconnect policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    initial: Duration,
    maximum: Duration,
}

impl RetryPolicy {
    /// Creates a bounded exponential backoff policy.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for zero delays or an inverted range.
    pub fn new(initial: Duration, maximum: Duration) -> Result<Self, GatewayError> {
        if initial.is_zero() || maximum.is_zero() || initial > maximum {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(Self { initial, maximum })
    }

    #[must_use]
    pub fn delay(self, attempt: u32) -> Duration {
        let factor = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.maximum)
    }
}

/// One statically-dispatched outbound relay adapter.
pub trait TunnelConnector: Clone + Send + Sync + 'static {
    fn connect_and_forward(
        &self,
        relay: &RelayEndpoint,
        local_addr: SocketAddr,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<(), GatewayError>> + Send;
}

/// rustls-backed outbound connector. No inbound public socket is created here.
#[derive(Clone)]
pub struct RustlsTunnelConnector {
    connector: TlsConnector,
    local_client: reqwest::Client,
}

impl RustlsTunnelConnector {
    /// Creates a mutually authenticated relay client using public `WebPKI` relay roots.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for invalid client certificate or key material.
    pub fn with_webpki_roots(
        client_certificate_pem: &[u8],
        client_private_key_pem: &[u8],
    ) -> Result<Self, GatewayError> {
        let roots = webpki_roots::TLS_SERVER_ROOTS
            .iter()
            .cloned()
            .collect::<RootCertStore>();
        Self::with_roots_and_identity(roots, client_certificate_pem, client_private_key_pem)
    }

    /// Creates a mutually authenticated relay client with an explicit trust root.
    /// This constructor supports private relays and the end-to-end TLS integration test.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for invalid CA, client certificate, or private key material.
    pub fn with_client_identity(
        root_certificate_pem: &[u8],
        client_certificate_pem: &[u8],
        client_private_key_pem: &[u8],
    ) -> Result<Self, GatewayError> {
        let root = CertificateDer::from_pem_slice(root_certificate_pem)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let mut roots = RootCertStore::empty();
        roots
            .add(root)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        Self::with_roots_and_identity(roots, client_certificate_pem, client_private_key_pem)
    }

    fn with_roots_and_identity(
        roots: RootCertStore,
        client_certificate_pem: &[u8],
        client_private_key_pem: &[u8],
    ) -> Result<Self, GatewayError> {
        let certificate = CertificateDer::from_pem_slice(client_certificate_pem)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let private_key = PrivateKeyDer::from_pem_slice(client_private_key_pem)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![certificate], private_key)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let local_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(FRAME_TIMEOUT)
            .build()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self {
            connector: TlsConnector::from(Arc::new(config)),
            local_client,
        })
    }
}

impl TunnelConnector for RustlsTunnelConnector {
    async fn connect_and_forward(
        &self,
        relay: &RelayEndpoint,
        local_addr: SocketAddr,
        cancellation: CancellationToken,
    ) -> Result<(), GatewayError> {
        if !local_addr.ip().is_loopback() {
            return Err(GatewayError::InvalidConfiguration);
        }
        let relay_stream = cancellable_timeout(
            &cancellation,
            TcpStream::connect((relay.host.as_str(), relay.port)),
        )
        .await?;
        let server_name = ServerName::try_from(relay.server_name.clone())
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let mut tls = cancellable_timeout(
            &cancellation,
            self.connector.connect(server_name, relay_stream),
        )
        .await?;
        write_tunnel_frame(
            &mut tls,
            &ClientFrame::Register {
                version: TUNNEL_PROTOCOL_VERSION,
                route_id: relay.route_id.clone(),
                public_host: relay.public_host.clone(),
                max_concurrent_requests: 1,
            },
        )
        .await?;
        let acknowledgement: RelayFrame = read_tunnel_frame(&mut tls).await?;
        if !matches!(
            acknowledgement,
            RelayFrame::Registered {
                version: TUNNEL_PROTOCOL_VERSION
            }
        ) {
            return Err(GatewayError::TunnelUnavailable);
        }
        loop {
            let request = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                result = read_tunnel_frame::<_, RelayFrame>(&mut tls) => result?,
            };
            let RelayFrame::Request(request) = request else {
                return Err(GatewayError::TunnelUnavailable);
            };
            let response = self.forward_local(local_addr, request).await?;
            write_tunnel_frame(&mut tls, &ClientFrame::Response(response)).await?;
        }
    }
}

impl RustlsTunnelConnector {
    async fn forward_local(
        &self,
        local_addr: SocketAddr,
        request: RelayRequest,
    ) -> Result<RelayResponse, GatewayError> {
        validate_relay_request(&request)?;
        let mut builder = self.local_client.post(format!("http://{local_addr}/mcp"));
        for (name, value) in request.headers {
            builder = builder.header(&name, value);
        }
        let mut response = builder
            .body(request.body)
            .send()
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_TUNNEL_FRAME_BYTES / 2 {
                return Err(GatewayError::TunnelUnavailable);
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(body).map_err(|_| GatewayError::TunnelUnavailable)?;
        Ok(RelayResponse {
            version: TUNNEL_PROTOCOL_VERSION,
            request_id: request.request_id,
            status,
            content_type,
            body,
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Register {
        version: u16,
        route_id: String,
        public_host: String,
        max_concurrent_requests: u8,
    },
    Response(RelayResponse),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayFrame {
    Registered { version: u16 },
    Request(RelayRequest),
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct RelayRequest {
    version: u16,
    request_id: uuid::Uuid,
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

#[derive(Serialize)]
struct RelayResponse {
    version: u16,
    request_id: uuid::Uuid,
    status: u16,
    content_type: Option<String>,
    body: String,
}

fn validate_relay_request(request: &RelayRequest) -> Result<(), GatewayError> {
    if request.version != TUNNEL_PROTOCOL_VERSION
        || request.request_id.get_version() != Some(uuid::Version::SortRand)
        || request.method != "POST"
        || request.path != "/mcp"
        || request.body.len() > 32 * 1024
        || request.headers.is_empty()
        || request.headers.len() > 3
        || !request.headers.contains_key("authorization")
        || request.headers.iter().any(|(name, value)| {
            !matches!(name.as_str(), "authorization" | "accept" | "content-type")
                || value.is_empty()
                || value.len() > MAX_HEADER_VALUE_BYTES
                || value.contains(['\r', '\n'])
        })
    {
        return Err(GatewayError::TunnelUnavailable);
    }
    Ok(())
}

async fn read_tunnel_frame<S, T>(stream: &mut S) -> Result<T, GatewayError>
where
    S: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    timeout(FRAME_TIMEOUT, async {
        let length = stream
            .read_u32_le()
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?;
        let length = usize::try_from(length).map_err(|_| GatewayError::TunnelUnavailable)?;
        if length == 0 || length > MAX_TUNNEL_FRAME_BYTES {
            return Err(GatewayError::TunnelUnavailable);
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| GatewayError::TunnelUnavailable)
    })
    .await
    .map_err(|_| GatewayError::TunnelUnavailable)?
}

async fn write_tunnel_frame<S, T>(stream: &mut S, value: &T) -> Result<(), GatewayError>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value).map_err(|_| GatewayError::TunnelUnavailable)?;
    if bytes.is_empty() || bytes.len() > MAX_TUNNEL_FRAME_BYTES {
        return Err(GatewayError::TunnelUnavailable);
    }
    timeout(FRAME_TIMEOUT, async {
        stream
            .write_u32_le(u32::try_from(bytes.len()).map_err(|_| GatewayError::TunnelUnavailable)?)
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)?;
        stream
            .flush()
            .await
            .map_err(|_| GatewayError::TunnelUnavailable)
    })
    .await
    .map_err(|_| GatewayError::TunnelUnavailable)?
}

/// Owns reconnect and cancellation for one outbound-only tunnel.
pub struct TunnelClient<C> {
    relay: RelayEndpoint,
    local_addr: SocketAddr,
    connector: C,
    retry: RetryPolicy,
}

impl<C: TunnelConnector> TunnelClient<C> {
    /// Creates an outbound client targeting one local loopback service.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` when the local destination is not loopback.
    pub fn new(
        relay: RelayEndpoint,
        local_addr: SocketAddr,
        connector: C,
        retry: RetryPolicy,
    ) -> Result<Self, GatewayError> {
        if !local_addr.ip().is_loopback() {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(Self {
            relay,
            local_addr,
            connector,
            retry,
        })
    }

    /// Reconnects with capped exponential delay until cancellation.
    ///
    /// # Errors
    /// Reserved for a future relay adapter failure that is not reconnectable. Current transport
    /// failures are retried until cancellation.
    pub async fn run(self, cancellation: CancellationToken) -> Result<(), GatewayError> {
        let mut attempt = 0_u32;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let result = self
                .connector
                .connect_and_forward(&self.relay, self.local_addr, cancellation.clone())
                .await;
            if cancellation.is_cancelled() {
                return Ok(());
            }
            if result.is_ok() {
                attempt = 0;
            }
            let delay = self.retry.delay(attempt);
            attempt = attempt.saturating_add(1);
            tracing::warn!(
                retry_delay_ms = delay.as_millis(),
                "outbound tunnel reconnect scheduled"
            );
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                () = sleep(delay) => {}
            }
        }
    }
}

impl<C> fmt::Debug for TunnelClient<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TunnelClient")
            .field("relay", &self.relay)
            .field("local_addr", &self.local_addr)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

async fn cancellable_timeout<T, E>(
    cancellation: &CancellationToken,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, GatewayError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(GatewayError::TunnelUnavailable),
        result = timeout(CONNECT_TIMEOUT, future) => {
            result.map_err(|_| GatewayError::TunnelUnavailable)?
                .map_err(|_| GatewayError::TunnelUnavailable)
        }
    }
}

fn valid_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.chars().any(char::is_whitespace)
        && !value.contains(['/', '\\', '@', ':'])
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::{RelayRequest, validate_relay_request};
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn relay_requests_require_uuid_v7_and_an_authorization_header() {
        let request = RelayRequest {
            version: 1,
            request_id: Uuid::now_v7(),
            method: "POST".to_owned(),
            path: "/mcp".to_owned(),
            headers: HashMap::from([(
                "authorization".to_owned(),
                "Bearer bounded-token".to_owned(),
            )]),
            body: "{}".to_owned(),
        };
        assert!(validate_relay_request(&request).is_ok());

        let mut missing_authorization = RelayRequest {
            headers: HashMap::new(),
            ..request
        };
        assert!(validate_relay_request(&missing_authorization).is_err());
        missing_authorization.request_id =
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("UUID v4 fixture");
        missing_authorization.headers.insert(
            "authorization".to_owned(),
            "Bearer bounded-token".to_owned(),
        );
        assert!(validate_relay_request(&missing_authorization).is_err());
    }
}
