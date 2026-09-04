use std::{collections::HashMap, fmt, sync::Arc};

use cortex_domain::PrincipalId;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;

use crate::GatewayError;

const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_CLAIM_BYTES: usize = 1024;

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
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
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
    keys: Arc<HashMap<String, OidcVerificationKey>>,
    pairings: Arc<HashMap<String, PrincipalId>>,
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
            keys: Arc::new(key_map),
            pairings: Arc::new(pairing_map),
        })
    }

    /// Authenticates all required token properties before consulting the local pairing map.
    ///
    /// # Errors
    /// Returns `InvalidToken` for any signature, algorithm, key, or claim failure and
    /// `UnpairedIdentity` only after successful validation of a subject without a pairing.
    #[allow(clippy::unused_async)] // Exact gateway contract permits future async OIDC key refresh.
    pub async fn resolve(&self, token: &BearerToken) -> Result<McpPrincipal, GatewayError> {
        let header = decode_header(token.as_str()).map_err(|_| GatewayError::InvalidToken)?;
        let key_id = header.kid.ok_or(GatewayError::InvalidToken)?;
        let key = self.keys.get(&key_id).ok_or(GatewayError::InvalidToken)?;
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
