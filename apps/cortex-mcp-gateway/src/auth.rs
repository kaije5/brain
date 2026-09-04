use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use cortex_domain::PrincipalId;
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header,
    jwk::{Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;

use crate::GatewayError;

const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_CLAIM_BYTES: usize = 1024;
const MAX_OIDC_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_JWKS_KEYS: usize = 32;
const MAX_PAIRED_SUBJECTS: usize = 64;
const MIN_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_CACHE_TTL: Duration = Duration::from_hours(24);
const FAILED_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);

/// A credential wrapper whose formatter never reveals the bearer value.
#[derive(Clone)]
pub struct BearerToken(String);

impl BearerToken {
    /// Validates a raw bearer token without decoding or logging it.
    ///
    /// # Errors
    /// Returns `InvalidToken` for an empty, oversized, or whitespace-containing credential.
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_TOKEN_BYTES
            || value.chars().any(char::is_whitespace)
        {
            return Err(GatewayError::InvalidToken);
        }
        Ok(Self(value))
    }

    pub(crate) fn from_authorization(value: &str) -> Result<Self, GatewayError> {
        let value = value
            .strip_prefix("Bearer ")
            .ok_or(GatewayError::InvalidToken)?;
        Self::new(value)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

/// Supported asymmetric OIDC token signature algorithms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum OidcAlgorithm {
    #[serde(rename = "RS256")]
    Rs256,
    #[serde(rename = "ES256")]
    Es256,
    #[serde(rename = "EdDSA")]
    EdDsa,
}

impl OidcAlgorithm {
    const fn jwt(self) -> Algorithm {
        match self {
            Self::Rs256 => Algorithm::RS256,
            Self::Es256 => Algorithm::ES256,
            Self::EdDsa => Algorithm::EdDSA,
        }
    }

    const fn jwk(self) -> KeyAlgorithm {
        match self {
            Self::Rs256 => KeyAlgorithm::RS256,
            Self::Es256 => KeyAlgorithm::ES256,
            Self::EdDsa => KeyAlgorithm::EdDSA,
        }
    }
}

/// The trusted identity-provider metadata used for claim validation.
#[derive(Clone, Debug)]
pub struct OidcMetadata {
    issuer: String,
    audience: String,
}

impl OidcMetadata {
    /// Creates the exact issuer and audience trust policy.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for empty, oversized, or control-bearing values.
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let issuer = issuer.into();
        let audience = audience.into();
        if !valid_oidc_issuer(&issuer) || !valid_claim_config(&audience) {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(Self { issuer, audience })
    }

    fn discovery_url(&self) -> String {
        format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        )
    }
}

/// Bounded OIDC document retrieval boundary. Implementations must return at most `max_bytes`.
pub trait OidcDocumentFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, GatewayError>> + Send + 'a>>;
}

/// HTTPS-only OIDC discovery and JWKS fetcher with redirects and ambient proxies disabled.
#[derive(Clone)]
pub struct HttpOidcFetcher {
    client: reqwest::Client,
}

impl HttpOidcFetcher {
    /// Creates a hardened metadata client.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` if the TLS client cannot be constructed.
    pub fn new() -> Result<Self, GatewayError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self { client })
    }
}

impl OidcDocumentFetcher for HttpOidcFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, GatewayError>> + Send + 'a>> {
        Box::pin(async move {
            let parsed = url::Url::parse(url).map_err(|_| GatewayError::OidcUnavailable)?;
            if !valid_https_url(&parsed) {
                return Err(GatewayError::OidcUnavailable);
            }
            let mut response = self
                .client
                .get(parsed)
                .send()
                .await
                .map_err(|_| GatewayError::OidcUnavailable)?;
            if !response.status().is_success()
                || response
                    .content_length()
                    .is_some_and(|length| length > max_bytes as u64)
            {
                return Err(GatewayError::OidcUnavailable);
            }
            let mut document = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| GatewayError::OidcUnavailable)?
            {
                if document.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(GatewayError::OidcUnavailable);
                }
                document.extend_from_slice(&chunk);
            }
            if document.is_empty() {
                return Err(GatewayError::OidcUnavailable);
            }
            Ok(document)
        })
    }
}

/// One pinned public OIDC verification key. Key material is redacted from debug output.
#[derive(Clone)]
pub struct OidcVerificationKey {
    key_id: String,
    algorithm: OidcAlgorithm,
    decoding_key: DecodingKey,
}

impl OidcVerificationKey {
    /// Parses one pinned asymmetric public verification key.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` when the key ID, size, algorithm, or PEM is invalid.
    pub fn from_pem(
        key_id: impl Into<String>,
        algorithm: OidcAlgorithm,
        pem: &[u8],
    ) -> Result<Self, GatewayError> {
        let key_id = key_id.into();
        if !valid_claim_config(&key_id) || pem.is_empty() || pem.len() > 64 * 1024 {
            return Err(GatewayError::InvalidConfiguration);
        }
        let decoding_key = match algorithm {
            OidcAlgorithm::Rs256 => DecodingKey::from_rsa_pem(pem),
            OidcAlgorithm::Es256 => DecodingKey::from_ec_pem(pem),
            OidcAlgorithm::EdDsa => DecodingKey::from_ed_pem(pem),
        }
        .map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self {
            key_id,
            algorithm,
            decoding_key,
        })
    }

    fn from_jwk(key_id: String, algorithm: OidcAlgorithm, jwk: &Jwk) -> Result<Self, GatewayError> {
        let intended_for_verification =
            match (&jwk.common.public_key_use, &jwk.common.key_operations) {
                (Some(PublicKeyUse::Signature) | None, None) => true,
                (None, Some(operations)) => operations.contains(&KeyOperations::Verify),
                _ => false,
            };
        if !valid_claim_config(&key_id)
            || jwk.common.key_algorithm != Some(algorithm.jwk())
            || !intended_for_verification
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        let decoding_key =
            DecodingKey::from_jwk(jwk).map_err(|_| GatewayError::InvalidConfiguration)?;
        Ok(Self {
            key_id,
            algorithm,
            decoding_key,
        })
    }
}

impl fmt::Debug for OidcVerificationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OidcVerificationKey")
            .field("key_id", &self.key_id)
            .field("algorithm", &self.algorithm)
            .field("key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Durable pairing from an OIDC subject to a daemon principal.
#[derive(Clone)]
pub struct PairedSubject {
    subject: String,
    principal_id: PrincipalId,
}

impl fmt::Debug for PairedSubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairedSubject")
            .field("subject", &"[REDACTED]")
            .field("principal_id", &"[REDACTED]")
            .finish()
    }
}

impl PairedSubject {
    /// Creates one durable subject-to-principal pairing.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for an invalid subject value.
    pub fn new(
        subject: impl Into<String>,
        principal_id: PrincipalId,
    ) -> Result<Self, GatewayError> {
        let subject = subject.into();
        if !valid_claim_config(&subject) {
            return Err(GatewayError::InvalidConfiguration);
        }
        Ok(Self {
            subject,
            principal_id,
        })
    }
}

/// An identity produced only after cryptographic OIDC validation and durable pairing lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpPrincipal {
    principal_id: PrincipalId,
}

impl McpPrincipal {
    #[must_use]
    pub const fn principal_id(self) -> PrincipalId {
        self.principal_id
    }
}

/// Strict OIDC verifier and subject-to-principal pairing resolver.
#[derive(Clone)]
pub struct PairedIdentityResolver {
    metadata: OidcMetadata,
    keys: Arc<RwLock<KeyCache>>,
    discovery: Option<Arc<DiscoveryState>>,
    pairings: Arc<HashMap<String, PrincipalId>>,
}

struct DiscoveryState {
    algorithms: Vec<OidcAlgorithm>,
    fetcher: Arc<dyn OidcDocumentFetcher>,
    cache_ttl: Duration,
    refresh: Mutex<()>,
}

struct KeyCache {
    keys: HashMap<String, OidcVerificationKey>,
    expires_at: Option<Instant>,
    last_forced_kid: Option<(String, Instant)>,
    retry_after: Option<Instant>,
}

impl PairedIdentityResolver {
    /// Creates a resolver while rejecting duplicate key IDs or subjects.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` when either trusted map contains a duplicate.
    pub fn new(
        metadata: OidcMetadata,
        keys: Vec<OidcVerificationKey>,
        pairings: Vec<PairedSubject>,
    ) -> Result<Self, GatewayError> {
        if keys.len() > MAX_JWKS_KEYS || pairings.len() > MAX_PAIRED_SUBJECTS {
            return Err(GatewayError::InvalidConfiguration);
        }
        let mut key_map = HashMap::new();
        for key in keys {
            if key_map.insert(key.key_id.clone(), key).is_some() {
                return Err(GatewayError::InvalidConfiguration);
            }
        }
        let mut pairing_map = HashMap::new();
        for pairing in pairings {
            if pairing_map
                .insert(pairing.subject, pairing.principal_id)
                .is_some()
            {
                return Err(GatewayError::InvalidConfiguration);
            }
        }
        Ok(Self {
            metadata,
            keys: Arc::new(RwLock::new(KeyCache {
                keys: key_map,
                expires_at: None,
                last_forced_kid: None,
                retry_after: None,
            })),
            discovery: None,
            pairings: Arc::new(pairing_map),
        })
    }

    /// Creates a resolver whose verification keys are loaded from issuer-anchored discovery.
    ///
    /// # Errors
    /// Returns `InvalidConfiguration` for empty algorithms, duplicate pairings, or unsafe TTLs.
    pub fn from_discovery(
        metadata: OidcMetadata,
        algorithms: Vec<OidcAlgorithm>,
        pairings: Vec<PairedSubject>,
        fetcher: Arc<dyn OidcDocumentFetcher>,
        cache_ttl: Duration,
    ) -> Result<Self, GatewayError> {
        if algorithms.is_empty()
            || !(MIN_CACHE_TTL..=MAX_CACHE_TTL).contains(&cache_ttl)
            || algorithms.iter().copied().collect::<HashSet<_>>().len() != algorithms.len()
        {
            return Err(GatewayError::InvalidConfiguration);
        }
        let mut resolver = Self::new(metadata, Vec::new(), pairings)?;
        resolver.discovery = Some(Arc::new(DiscoveryState {
            algorithms,
            fetcher,
            cache_ttl,
            refresh: Mutex::new(()),
        }));
        Ok(resolver)
    }

    /// Authenticates all required token properties before consulting the local pairing map.
    ///
    /// # Errors
    /// Returns `InvalidToken` for any signature, algorithm, key, or claim failure and
    /// `UnpairedIdentity` only after successful validation of a subject without a pairing.
    pub async fn resolve(&self, token: &BearerToken) -> Result<McpPrincipal, GatewayError> {
        let header = decode_header(token.as_str()).map_err(|_| GatewayError::InvalidToken)?;
        let key_id = header.kid.ok_or(GatewayError::InvalidToken)?;
        if !valid_claim_config(&key_id) {
            return Err(GatewayError::InvalidToken);
        }
        let key = self.key_for(&key_id).await?;
        if header.alg != key.algorithm.jwt() {
            return Err(GatewayError::InvalidToken);
        }
        let mut validation = Validation::new(key.algorithm.jwt());
        validation.set_audience(&[self.metadata.audience.as_str()]);
        validation.set_issuer(&[self.metadata.issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        validation.validate_exp = true;
        validation.validate_aud = true;
        validation.leeway = 30;
        let claims = decode::<Claims>(token.as_str(), &key.decoding_key, &validation)
            .map_err(|_| GatewayError::InvalidToken)?
            .claims;
        if !valid_claim_config(&claims.sub) {
            return Err(GatewayError::InvalidToken);
        }
        let principal_id = self
            .pairings
            .get(&claims.sub)
            .copied()
            .ok_or(GatewayError::UnpairedIdentity)?;
        Ok(McpPrincipal { principal_id })
    }

    async fn key_for(&self, key_id: &str) -> Result<OidcVerificationKey, GatewayError> {
        let now = Instant::now();
        {
            let cache = self.keys.read().await;
            if cache.expires_at.is_none_or(|expires| expires > now)
                && let Some(key) = cache.keys.get(key_id)
            {
                return Ok(key.clone());
            }
        }
        let Some(discovery) = &self.discovery else {
            return Err(GatewayError::InvalidToken);
        };
        let _refresh = discovery.refresh.lock().await;
        let now = Instant::now();
        {
            let cache = self.keys.read().await;
            if cache.expires_at.is_some_and(|expires| expires > now) {
                if let Some(key) = cache.keys.get(key_id) {
                    return Ok(key.clone());
                }
                if cache
                    .last_forced_kid
                    .as_ref()
                    .is_some_and(|(kid, until)| kid == key_id && *until > now)
                {
                    return Err(GatewayError::InvalidToken);
                }
            }
            if cache.retry_after.is_some_and(|until| until > now) {
                return cache
                    .keys
                    .get(key_id)
                    .cloned()
                    .ok_or(GatewayError::InvalidToken);
            }
        }
        if let Ok(keys) = self.refresh_keys(discovery).await {
            let key = keys.get(key_id).cloned();
            let mut cache = self.keys.write().await;
            cache.keys = keys;
            cache.expires_at = Some(now + discovery.cache_ttl);
            cache.retry_after = None;
            cache.last_forced_kid = key
                .is_none()
                .then(|| (key_id.to_owned(), now + FAILED_REFRESH_COOLDOWN));
            key.ok_or(GatewayError::InvalidToken)
        } else {
            let mut cache = self.keys.write().await;
            cache.retry_after = Some(now + FAILED_REFRESH_COOLDOWN);
            cache
                .keys
                .get(key_id)
                .cloned()
                .ok_or(GatewayError::InvalidToken)
        }
    }

    async fn refresh_keys(
        &self,
        discovery: &DiscoveryState,
    ) -> Result<HashMap<String, OidcVerificationKey>, GatewayError> {
        let metadata_bytes = discovery
            .fetcher
            .fetch(&self.metadata.discovery_url(), MAX_OIDC_DOCUMENT_BYTES)
            .await?;
        let document: DiscoveryDocument = serde_json::from_slice(&metadata_bytes)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        if document.issuer != self.metadata.issuer {
            return Err(GatewayError::InvalidConfiguration);
        }
        let issuer = url::Url::parse(&self.metadata.issuer)
            .map_err(|_| GatewayError::InvalidConfiguration)?;
        let jwks_url =
            url::Url::parse(&document.jwks_uri).map_err(|_| GatewayError::InvalidConfiguration)?;
        if !same_https_origin(&issuer, &jwks_url) {
            return Err(GatewayError::InvalidConfiguration);
        }
        let jwks_bytes = discovery
            .fetcher
            .fetch(jwks_url.as_str(), MAX_OIDC_DOCUMENT_BYTES)
            .await?;
        let jwks: JwkSet =
            serde_json::from_slice(&jwks_bytes).map_err(|_| GatewayError::InvalidConfiguration)?;
        if jwks.keys.is_empty() || jwks.keys.len() > MAX_JWKS_KEYS {
            return Err(GatewayError::InvalidConfiguration);
        }
        let mut keys = HashMap::new();
        for jwk in &jwks.keys {
            let key_id = jwk
                .common
                .key_id
                .clone()
                .ok_or(GatewayError::InvalidConfiguration)?;
            let algorithm = discovery
                .algorithms
                .iter()
                .copied()
                .find(|algorithm| jwk.common.key_algorithm == Some(algorithm.jwk()))
                .ok_or(GatewayError::InvalidConfiguration)?;
            let key = OidcVerificationKey::from_jwk(key_id.clone(), algorithm, jwk)?;
            if keys.insert(key_id, key).is_some() {
                return Err(GatewayError::InvalidConfiguration);
            }
        }
        Ok(keys)
    }
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
}

fn valid_claim_config(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_CLAIM_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_oidc_issuer(value: &str) -> bool {
    if !valid_claim_config(value) {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn valid_https_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn same_https_origin(issuer: &url::Url, candidate: &url::Url) -> bool {
    valid_https_url(candidate)
        && issuer.scheme() == candidate.scheme()
        && issuer.host_str() == candidate.host_str()
        && issuer.port_or_known_default() == candidate.port_or_known_default()
}
