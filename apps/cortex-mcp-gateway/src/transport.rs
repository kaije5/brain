use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any_service,
};
use cortex_domain::PrincipalId;
use cortex_mcp::{HttpSecurityConfig, McpPrincipal as LocalMcpPrincipal, streamable_http_service};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    BearerToken, GatewayError, OidcAlgorithm, OidcMetadata, OidcVerificationKey,
    PairedIdentityResolver, PairedSubject, RelayEndpoint,
};

/// File-backed gateway configuration. The public-listen field is intentionally always absent.
#[derive(Clone)]
pub struct GatewayConfig {
    local_port: u16,
    pub public_listen_addr: Option<SocketAddr>,
    oidc: OidcFile,
    paired_subjects: Vec<PairedSubjectFile>,
    relay: RelayEndpoint,
}

impl GatewayConfig {
    /// Parses a closed JSON configuration schema. A public listen option is rejected as unknown.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for malformed, oversized, unknown, or unsafe settings.
    pub fn parse(input: &str) -> Result<Self, GatewayError> {
        if input.is_empty() || input.len() > 256 * 1024 {
            return Err(GatewayError::InvalidConfiguration);
        }
        let file: GatewayFile =
            serde_json::from_str(input).map_err(|_| GatewayError::InvalidConfiguration)?;
        let relay = RelayEndpoint::new(file.relay.host, file.relay.port, file.relay.server_name)?;
        Ok(Self {
            local_port: file.local_port,
            public_listen_addr: None,
            oidc: file.oidc,
            paired_subjects: file.paired_subjects,
            relay,
        })
    }

    #[must_use]
    pub const fn local_bind_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.local_port)
    }

    #[must_use]
    pub const fn relay(&self) -> &RelayEndpoint {
        &self.relay
    }

    /// Builds the verifier and durable subject map without exposing key material.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for missing or invalid OIDC keys and pairings.
    pub fn identity_resolver(&self) -> Result<PairedIdentityResolver, GatewayError> {
        let metadata = OidcMetadata::new(&self.oidc.issuer, &self.oidc.audience)?;
        let keys = self
            .oidc
            .keys
            .iter()
            .map(|key| {
                OidcVerificationKey::from_pem(
                    &key.key_id,
                    key.algorithm,
                    key.public_key_pem.as_bytes(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pairings = self
            .paired_subjects
            .iter()
            .map(|pairing| {
                let principal_id = PrincipalId::try_from(pairing.principal_id)
                    .map_err(|_| GatewayError::InvalidConfiguration)?;
                PairedSubject::new(&pairing.subject, principal_id)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if keys.is_empty() || pairings.is_empty() {
            return Err(GatewayError::InvalidConfiguration);
        }
        PairedIdentityResolver::new(metadata, keys, pairings)
    }

    #[must_use]
    pub fn paired_principal_ids(&self) -> Vec<PrincipalId> {
        self.paired_subjects
            .iter()
            .filter_map(|pairing| PrincipalId::try_from(pairing.principal_id).ok())
            .collect()
    }
}

impl fmt::Debug for GatewayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayConfig")
            .field("local_port", &self.local_port)
            .field("local_bind_addr", &self.local_bind_addr())
            .field("public_listen_addr", &self.public_listen_addr)
            .field("oidc", &"[REDACTED]")
            .field("paired_subject_count", &self.paired_subjects.len())
            .field("relay", &self.relay)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayFile {
    local_port: u16,
    oidc: OidcFile,
    paired_subjects: Vec<PairedSubjectFile>,
    relay: RelayFile,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcFile {
    issuer: String,
    audience: String,
    keys: Vec<OidcKeyFile>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcKeyFile {
    #[serde(rename = "kid")]
    key_id: String,
    algorithm: OidcAlgorithm,
    public_key_pem: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedSubjectFile {
    subject: String,
    principal_id: uuid::Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayFile {
    host: String,
    port: u16,
    server_name: String,
}

/// Request-local mapping from an authenticated remote principal to its paired daemon client.
#[derive(Clone, Default)]
pub struct PrincipalRegistry(Arc<HashMap<PrincipalId, LocalMcpPrincipal>>);

impl PrincipalRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, principal_id: PrincipalId, principal: LocalMcpPrincipal) {
        Arc::make_mut(&mut self.0).insert(principal_id, principal);
    }

    fn get(&self, principal_id: PrincipalId) -> Option<LocalMcpPrincipal> {
        self.0.get(&principal_id).cloned()
    }
}

#[derive(Clone)]
struct AuthState {
    resolver: PairedIdentityResolver,
    principals: PrincipalRegistry,
}

/// Authenticated local HTTP adapter. It never owns storage or authorization policy.
pub struct GatewayTransport {
    router: Router,
}

impl GatewayTransport {
    #[must_use]
    pub fn new(
        resolver: PairedIdentityResolver,
        principals: PrincipalRegistry,
        cancellation: CancellationToken,
    ) -> Self {
        let mcp = streamable_http_service(&HttpSecurityConfig::loopback(cancellation));
        let state = AuthState {
            resolver,
            principals,
        };
        let router = Router::new()
            .route("/mcp", any_service(mcp))
            .route_layer(middleware::from_fn_with_state(state, authenticate));
        Self { router }
    }

    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Serves only on a caller-supplied, already-bound loopback listener.
    ///
    /// # Errors
    /// Returns a redacted configuration or local transport category.
    pub async fn serve(
        self,
        listener: TcpListener,
        cancellation: CancellationToken,
    ) -> Result<(), GatewayError> {
        let address = listener
            .local_addr()
            .map_err(|_| GatewayError::LocalTransportUnavailable)?;
        if !address.ip().is_loopback() {
            return Err(GatewayError::InvalidConfiguration);
        }
        axum::serve(listener, self.router)
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .await
            .map_err(|_| GatewayError::LocalTransportUnavailable)
    }
}

/// Binds the only inbound listener to an explicit IPv4 loopback address.
///
/// # Errors
/// Returns `LocalTransportUnavailable` when the loopback socket cannot be bound.
pub async fn bind_loopback(config: &GatewayConfig) -> Result<TcpListener, GatewayError> {
    TcpListener::bind(config.local_bind_addr())
        .await
        .map_err(|_| GatewayError::LocalTransportUnavailable)
}

async fn authenticate(
    State(state): State<AuthState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let result = async {
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(GatewayError::InvalidToken)?;
        let token = BearerToken::from_authorization(authorization)?;
        let remote = state.resolver.resolve(&token).await?;
        let principal = state
            .principals
            .get(remote.principal_id())
            .ok_or(GatewayError::UnpairedIdentity)?;
        request.extensions_mut().insert(principal);
        Ok::<_, GatewayError>(next.run(request).await)
    }
    .await;
    match result {
        Ok(response) => response,
        Err(_) => safe_unauthorized(),
    }
}

fn safe_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(
            r#"{"error":{"code":"cortex_unauthorized","message":"Authentication failed."}}"#,
        ),
    )
        .into_response()
}
