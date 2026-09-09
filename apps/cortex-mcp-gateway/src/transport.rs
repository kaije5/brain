use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::File,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
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
use tokio::{net::TcpListener, sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    BearerToken, GatewayError, HttpOidcFetcher, OidcAlgorithm, OidcMetadata,
    PairedIdentityResolver, PairedSubject, RelayEndpoint, RustlsTunnelConnector,
};

/// File-backed gateway configuration. The public-listen field is intentionally always absent.
#[derive(Clone)]
pub struct GatewayConfig {
    local_port: u16,
    pub public_listen_addr: Option<SocketAddr>,
    oidc: OidcFile,
    paired_subjects: Vec<PairedSubjectFile>,
    relay: RelayEndpoint,
    relay_client_certificate_path: PathBuf,
    relay_client_private_key_path: PathBuf,
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
        let relay = RelayEndpoint::new(
            file.relay.host,
            file.relay.port,
            file.relay.server_name,
            file.relay.route_id,
            file.relay.public_host,
        )?;
        let relay_client_certificate_path = valid_path(file.relay.client_certificate_path)?;
        let relay_client_private_key_path = valid_path(file.relay.client_private_key_path)?;
        Ok(Self {
            local_port: file.local_port,
            public_listen_addr: None,
            oidc: file.oidc,
            paired_subjects: file.paired_subjects,
            relay,
            relay_client_certificate_path,
            relay_client_private_key_path,
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
        let pairings = self
            .paired_subjects
            .iter()
            .map(|pairing| {
                let principal_id = PrincipalId::try_from(pairing.principal_id)
                    .map_err(|_| GatewayError::InvalidConfiguration)?;
                PairedSubject::new(&pairing.subject, principal_id)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if pairings.is_empty() {
            return Err(GatewayError::InvalidConfiguration);
        }
        let ttl = Duration::from_secs(self.oidc.cache_ttl_seconds);
        PairedIdentityResolver::from_discovery(
            metadata,
            self.oidc.algorithms.clone(),
            pairings,
            Arc::new(HttpOidcFetcher::new()?),
            ttl,
        )
    }

    #[must_use]
    pub fn paired_principal_ids(&self) -> Vec<PrincipalId> {
        self.paired_subjects
            .iter()
            .filter_map(|pairing| PrincipalId::try_from(pairing.principal_id).ok())
            .collect()
    }

    /// Loads the exact per-principal IPC enrollments referenced by the paired subjects.
    ///
    /// # Errors
    /// Returns a redacted configuration error if an enrollment is missing or maps to another
    /// principal.
    pub fn paired_ipc_clients(
        &self,
    ) -> Result<Vec<(PrincipalId, cortexd::AuthenticatedIpcClient)>, GatewayError> {
        let mut seen = HashSet::new();
        let mut clients = Vec::with_capacity(self.paired_subjects.len());
        for pairing in &self.paired_subjects {
            let expected = PrincipalId::try_from(pairing.principal_id)
                .map_err(|_| GatewayError::InvalidConfiguration)?;
            if !seen.insert(expected) {
                return Err(GatewayError::InvalidConfiguration);
            }
            let client =
                cortexd::AuthenticatedIpcClient::from_enrollment_path(&pairing.ipc_enrollment_path)
                    .map_err(|_| GatewayError::InvalidConfiguration)?;
            if client.principal_id() != expected {
                return Err(GatewayError::InvalidConfiguration);
            }
            clients.push((expected, client));
        }
        Ok(clients)
    }

    /// Builds the mutually authenticated outbound relay connector from bounded local files.
    ///
    /// # Errors
    /// Returns a redacted configuration error for missing, oversized, or invalid identity files.
    pub fn tunnel_connector(&self) -> Result<RustlsTunnelConnector, GatewayError> {
        let certificate = read_bounded(&self.relay_client_certificate_path)?;
        let private_key = read_bounded(&self.relay_client_private_key_path)?;
        RustlsTunnelConnector::with_webpki_roots(&certificate, &private_key)
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
    algorithms: Vec<OidcAlgorithm>,
    cache_ttl_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedSubjectFile {
    subject: String,
    principal_id: uuid::Uuid,
    ipc_enrollment_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayFile {
    host: String,
    port: u16,
    server_name: String,
    route_id: String,
    public_host: String,
    client_certificate_path: String,
    client_private_key_path: String,
}

fn valid_path(value: String) -> Result<PathBuf, GatewayError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(GatewayError::InvalidConfiguration);
    }
    Ok(PathBuf::from(value))
}

fn read_bounded(path: &PathBuf) -> Result<Vec<u8>, GatewayError> {
    let file = File::open(path).map_err(|_| GatewayError::InvalidConfiguration)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| GatewayError::InvalidConfiguration)?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return Err(GatewayError::InvalidConfiguration);
    }
    Ok(bytes)
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
    rate_limiter: PrincipalRateLimiter,
}

/// Fixed-window rate bound applied after cryptographic identity resolution and before MCP parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayRateLimit {
    requests: u32,
    window: Duration,
}

impl GatewayRateLimit {
    /// Creates a bounded rate limit.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for zero or unreasonably large limits.
    pub fn new(requests: u32, window: Duration) -> Result<Self, GatewayError> {
        if requests == 0
            || requests > 10_000
            || !(Duration::from_millis(100)..=Duration::from_hours(1)).contains(&window)
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(Self { requests, window })
    }
}

#[derive(Clone)]
struct PrincipalRateLimiter {
    config: GatewayRateLimit,
    windows: Arc<Mutex<HashMap<PrincipalId, RateWindow>>>,
}

struct RateWindow {
    started_at: Instant,
    requests: u32,
}

impl PrincipalRateLimiter {
    fn new(config: GatewayRateLimit) -> Self {
        Self {
            config,
            windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn allow(&self, principal_id: PrincipalId) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().await;
        let window = windows.entry(principal_id).or_insert(RateWindow {
            started_at: now,
            requests: 0,
        });
        if now.duration_since(window.started_at) >= self.config.window {
            window.started_at = now;
            window.requests = 0;
        }
        if window.requests >= self.config.requests {
            return false;
        }
        window.requests = window.requests.saturating_add(1);
        true
    }
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
        Self::new_with_rate_limit(
            resolver,
            principals,
            cancellation,
            GatewayRateLimit {
                requests: 120,
                window: Duration::from_mins(1),
            },
        )
    }

    #[must_use]
    pub fn new_with_rate_limit(
        resolver: PairedIdentityResolver,
        principals: PrincipalRegistry,
        cancellation: CancellationToken,
        rate_limit: GatewayRateLimit,
    ) -> Self {
        let mcp = streamable_http_service(&HttpSecurityConfig::loopback(cancellation));
        let state = AuthState {
            resolver,
            principals,
            rate_limiter: PrincipalRateLimiter::new(rate_limit),
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
        if !state.rate_limiter.allow(remote.principal_id()).await {
            return Ok::<_, GatewayError>(safe_rate_limited());
        }
        request.extensions_mut().insert(principal);
        Ok::<_, GatewayError>(next.run(request).await)
    }
    .await;
    match result {
        Ok(response) => response,
        Err(_) => safe_unauthorized(),
    }
}

fn safe_rate_limited() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(
            r#"{"error":{"code":"cortex_rate_limited","message":"Request rate exceeded."}}"#,
        ),
    )
        .into_response()
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
