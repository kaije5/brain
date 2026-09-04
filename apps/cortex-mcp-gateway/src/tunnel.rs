use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use tokio::{
    io::copy_bidirectional,
    net::TcpStream,
    time::{sleep, timeout},
};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::GatewayError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Vendor-neutral destination for the stateless relay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayEndpoint {
    host: String,
    port: u16,
    server_name: String,
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
    ) -> Result<Self, GatewayError> {
        let host = host.into();
        let server_name = server_name.into();
        if !valid_host(&host) || !valid_host(&server_name) || port == 0 {
            return Err(GatewayError::InvalidConfiguration);
        }
        ServerName::try_from(server_name.clone())
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self {
            host,
            port,
            server_name,
        })
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
}

impl RustlsTunnelConnector {
    #[must_use]
    pub fn with_webpki_roots() -> Self {
        let roots = webpki_roots::TLS_SERVER_ROOTS
            .iter()
            .cloned()
            .collect::<RootCertStore>();
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Self {
            connector: TlsConnector::from(Arc::new(config)),
        }
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
        let mut local = cancellable_timeout(&cancellation, TcpStream::connect(local_addr)).await?;
        tokio::select! {
            () = cancellation.cancelled() => Ok(()),
            result = copy_bidirectional(&mut tls, &mut local) => {
                result.map(|_| ()).map_err(|_| GatewayError::TunnelUnavailable)
            }
        }
    }
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
