#![forbid(unsafe_code)]

mod auth;
mod transport;
mod tunnel;

pub use auth::{
    BearerToken, HttpOidcFetcher, McpPrincipal, OidcAlgorithm, OidcDocumentFetcher, OidcMetadata,
    OidcVerificationKey, PairedIdentityResolver, PairedSubject,
};
pub use transport::{
    GatewayConfig, GatewayRateLimit, GatewayTransport, PrincipalRegistry, bind_loopback,
};
pub use tunnel::{
    ConnectionHealth, RelayEndpoint, RetryPolicy, RustlsTunnelConnector, TunnelClient,
    TunnelConnector,
};

/// Stable, non-sensitive gateway failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayError {
    InvalidConfiguration,
    InvalidToken,
    UnpairedIdentity,
    LocalTransportUnavailable,
    TunnelUnavailable,
    OidcUnavailable,
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "invalid gateway configuration",
            Self::InvalidToken => "remote identity could not be authenticated",
            Self::UnpairedIdentity => "remote identity is not paired",
            Self::LocalTransportUnavailable => "local gateway transport is unavailable",
            Self::TunnelUnavailable => "outbound tunnel is unavailable",
            Self::OidcUnavailable => "identity metadata is unavailable",
        })
    }
}

impl std::error::Error for GatewayError {}
