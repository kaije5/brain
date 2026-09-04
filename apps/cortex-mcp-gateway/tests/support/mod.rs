use std::time::{SystemTime, UNIX_EPOCH};

use cortex_domain::PrincipalId;
use cortex_mcp_gateway::{
    BearerToken, OidcAlgorithm, OidcMetadata, OidcVerificationKey, PairedIdentityResolver,
    PairedSubject,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::Serialize;

const PRIVATE_KEY: &[u8] = br"-----BEGIN PRIVATE KEY-----
MHICAQEwBQYDK2VwBCIEINTuctv5E1hK1bbY8fdp+K06/nwoy/HU++CXqI9EdVhC
oB8wHQYKKoZIhvcNAQkJFDEPDA1DdXJkbGUgQ2hhaXJzgSEAGb9ECWmEzf6FQbrB
Z9w7lshQhqowtrbLDFw4rXAxZuE=
-----END PRIVATE KEY-----
";
const PUBLIC_KEY: &[u8] = br"-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=
-----END PUBLIC KEY-----
";

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    aud: &'a str,
    sub: &'a str,
    exp: u64,
}

/// Builds a resolver backed by the deterministic test key and one pairing.
///
/// # Panics
///
/// Panics if the checked-in test fixtures are invalid.
#[must_use]
pub fn resolver(subject: &str, principal_id: PrincipalId) -> PairedIdentityResolver {
    PairedIdentityResolver::new(
        metadata(),
        vec![verification_key()],
        vec![PairedSubject::new(subject, principal_id).expect("valid pairing")],
    )
    .expect("valid resolver")
}

/// Returns the deterministic test issuer metadata.
///
/// # Panics
///
/// Panics if the checked-in issuer metadata is invalid.
#[must_use]
pub fn metadata() -> OidcMetadata {
    OidcMetadata::new("https://issuer.example", "cortex").expect("valid metadata")
}

/// Returns the deterministic test verification key.
///
/// # Panics
///
/// Panics if the checked-in public key is invalid.
#[must_use]
pub fn verification_key() -> OidcVerificationKey {
    OidcVerificationKey::from_pem("test-key", OidcAlgorithm::EdDsa, PUBLIC_KEY)
        .expect("valid verification key")
}

/// Signs a bounded bearer token with the deterministic test key.
///
/// # Panics
///
/// Panics if the checked-in key is invalid or token encoding fails.
#[must_use]
pub fn token(subject: &str, issuer: &str, audience: &str, offset_seconds: i64) -> BearerToken {
    BearerToken::new(encoded_token(subject, issuer, audience, offset_seconds))
        .expect("bounded token")
}

/// Signs and returns a raw token for request-level tests.
///
/// # Panics
///
/// Panics if the system clock predates the Unix epoch, the checked-in key is invalid, or token
/// encoding fails.
#[must_use]
pub fn encoded_token(subject: &str, issuer: &str, audience: &str, offset_seconds: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let exp = if offset_seconds.is_negative() {
        now.saturating_sub(offset_seconds.unsigned_abs())
    } else {
        now.saturating_add(offset_seconds.unsigned_abs())
    };
    let mut header = Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.kid = Some("test-key".to_owned());
    encode(
        &header,
        &Claims {
            iss: issuer,
            aud: audience,
            sub: subject,
            exp,
        },
        &EncodingKey::from_ed_pem(PRIVATE_KEY).expect("valid test key"),
    )
    .expect("test token")
}
