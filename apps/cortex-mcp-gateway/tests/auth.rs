use cortex_domain::PrincipalId;
use cortex_mcp_gateway::{
    BearerToken, GatewayError, OidcAlgorithm, OidcDocumentFetcher, OidcMetadata,
    PairedIdentityResolver, PairedSubject,
};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub mod support;
use support::{encoded_token, metadata, resolver, token, verification_key};

#[tokio::test]
async fn token_for_unpaired_subject_is_rejected_before_mcp_dispatch() {
    let resolver = resolver("paired-subject", PrincipalId::new());
    let token = token("unknown-subject", "https://issuer.example", "cortex", 300);
    assert_eq!(
        resolver.resolve(&token).await,
        Err(GatewayError::UnpairedIdentity)
    );
}

#[tokio::test]
async fn valid_oidc_token_maps_only_to_the_durable_paired_principal() {
    let principal_id = PrincipalId::new();
    let resolver = PairedIdentityResolver::new(
        metadata(),
        vec![verification_key()],
        vec![PairedSubject::new("paired-subject", principal_id).expect("valid pairing")],
    )
    .expect("valid resolver");
    let token = token("paired-subject", "https://issuer.example", "cortex", 300);
    let identity = resolver.resolve(&token).await.expect("valid token");
    assert_eq!(identity.principal_id(), principal_id);
}

#[tokio::test]
async fn issuer_audience_signature_expiry_and_subject_are_all_required() {
    let resolver = resolver("paired-subject", PrincipalId::new());
    let invalid = [
        token("paired-subject", "https://wrong.example", "cortex", 300),
        token(
            "paired-subject",
            "https://issuer.example",
            "wrong-audience",
            300,
        ),
        token("paired-subject", "https://issuer.example", "cortex", -300),
        token("", "https://issuer.example", "cortex", 300),
    ];
    for token in invalid {
        assert_eq!(
            resolver.resolve(&token).await,
            Err(GatewayError::InvalidToken)
        );
    }

    let mut corrupted = encoded_token("paired-subject", "https://issuer.example", "cortex", 300);
    corrupted.push('x');
    let corrupted = BearerToken::new(corrupted).expect("bounded token");
    assert_eq!(
        resolver.resolve(&corrupted).await,
        Err(GatewayError::InvalidToken)
    );
}

#[test]
fn bearer_debug_output_never_contains_the_credential() {
    let token = BearerToken::new("header.payload.signature").expect("bounded token");
    let rendered = format!("{token:?}");
    assert!(rendered.contains("REDACTED"));
    assert!(!rendered.contains("payload"));
}

#[test]
fn paired_subject_debug_output_never_contains_the_remote_identity() {
    let pairing =
        PairedSubject::new("sensitive-remote-subject", PrincipalId::new()).expect("valid pairing");
    let rendered = format!("{pairing:?}");
    assert!(rendered.contains("REDACTED"));
    assert!(!rendered.contains("sensitive-remote-subject"));
}

#[test]
fn oidc_issuer_must_use_https_without_embedded_credentials() {
    for issuer in [
        "http://issuer.example",
        "https://user:secret@issuer.example",
        "https://issuer.example/path#fragment",
    ] {
        assert!(matches!(
            OidcMetadata::new(issuer, "cortex"),
            Err(GatewayError::InvalidConfiguration)
        ));
    }
}

#[test]
fn resolver_rejects_an_unbounded_number_of_paired_subjects() {
    let pairings = (0..65)
        .map(|index| {
            PairedSubject::new(format!("subject-{index}"), PrincipalId::new()).expect("pairing")
        })
        .collect();
    assert!(matches!(
        PairedIdentityResolver::new(metadata(), vec![verification_key()], pairings),
        Err(GatewayError::InvalidConfiguration)
    ));
}

#[tokio::test]
async fn discovery_refreshes_jwks_once_on_rotation_and_concurrent_kid_miss() {
    let fetcher = FakeFetcher::new();
    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(jwks_url(), Ok(jwks("first-key")));
    let resolver = discovered_resolver(fetcher.clone());
    let first = support::token_with_kid(
        "paired-subject",
        "https://issuer.example",
        "cortex",
        300,
        "first-key",
    );
    assert!(resolver.resolve(&first).await.is_ok());

    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(jwks_url(), Ok(jwks("rotated-key")));
    let rotated = support::token_with_kid(
        "paired-subject",
        "https://issuer.example",
        "cortex",
        300,
        "rotated-key",
    );
    let (a, b, c) = tokio::join!(
        resolver.resolve(&rotated),
        resolver.resolve(&rotated),
        resolver.resolve(&rotated)
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(fetcher.calls(), 4);
}

#[tokio::test(start_paused = true)]
async fn ttl_refresh_failure_retains_last_known_good_keys() {
    let fetcher = FakeFetcher::new();
    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(jwks_url(), Ok(jwks("test-key")));
    let resolver = discovered_resolver(fetcher.clone());
    let token = token("paired-subject", "https://issuer.example", "cortex", 300);
    assert!(resolver.resolve(&token).await.is_ok());

    tokio::time::advance(Duration::from_secs(61)).await;
    fetcher.push(discovery_url(), Err(GatewayError::OidcUnavailable));
    assert!(resolver.resolve(&token).await.is_ok());
}

#[tokio::test]
async fn discovery_rejects_issuer_drift_cross_origin_jwks_and_oversized_documents() {
    for invalid_discovery in [
        discovery("https://other.example", jwks_url()),
        discovery("https://issuer.example", "https://keys.other.example/jwks"),
    ] {
        let fetcher = FakeFetcher::new();
        fetcher.push(discovery_url(), Ok(invalid_discovery));
        let resolver = discovered_resolver(fetcher);
        let token = token("paired-subject", "https://issuer.example", "cortex", 300);
        assert_eq!(
            resolver.resolve(&token).await,
            Err(GatewayError::InvalidToken)
        );
    }

    let fetcher = FakeFetcher::new();
    fetcher.push(discovery_url(), Ok(vec![b'x'; 65 * 1024]));
    let resolver = discovered_resolver(fetcher);
    let token = token("paired-subject", "https://issuer.example", "cortex", 300);
    assert_eq!(
        resolver.resolve(&token).await,
        Err(GatewayError::InvalidToken)
    );
}

#[tokio::test]
async fn discovery_accepts_standard_extra_metadata_but_rejects_unknown_kid_after_one_refresh() {
    let fetcher = FakeFetcher::new();
    fetcher.push(
        discovery_url(),
        Ok(serde_json::to_vec(&serde_json::json!({
            "issuer":"https://issuer.example", "jwks_uri":jwks_url(),
            "authorization_endpoint":"https://issuer.example/authorize",
            "response_types_supported":["code"]
        }))
        .expect("fixture")),
    );
    fetcher.push(jwks_url(), Ok(jwks("known-key")));
    let resolver = discovered_resolver(fetcher.clone());
    let unknown = support::token_with_kid(
        "paired-subject",
        "https://issuer.example",
        "cortex",
        300,
        "unknown-key",
    );
    assert_eq!(
        resolver.resolve(&unknown).await,
        Err(GatewayError::InvalidToken)
    );
    assert_eq!(
        resolver.resolve(&unknown).await,
        Err(GatewayError::InvalidToken)
    );
    assert_eq!(fetcher.calls(), 2);
}

#[tokio::test]
async fn malformed_jwks_is_rejected_without_replacing_a_valid_cache() {
    let fetcher = FakeFetcher::new();
    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(jwks_url(), Ok(jwks("test-key")));
    let resolver = discovered_resolver(fetcher.clone());
    let known = token("paired-subject", "https://issuer.example", "cortex", 300);
    assert!(resolver.resolve(&known).await.is_ok());

    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(
        jwks_url(),
        Ok(br#"{"keys":[{"kid":"bad","alg":"EdDSA"}]}"#.to_vec()),
    );
    let missing = support::token_with_kid(
        "paired-subject",
        "https://issuer.example",
        "cortex",
        300,
        "bad",
    );
    assert_eq!(
        resolver.resolve(&missing).await,
        Err(GatewayError::InvalidToken)
    );
    assert!(resolver.resolve(&known).await.is_ok());
}

#[tokio::test]
async fn jwks_accepts_verify_key_ops_without_use_and_rejects_non_verification_keys() {
    let verifying = serde_json::to_vec(&serde_json::json!({"keys":[{
        "kty":"OKP", "crv":"Ed25519",
        "x":"Gb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE",
        "alg":"EdDSA", "kid":"test-key", "key_ops":["verify"]
    }]}))
    .expect("fixture");
    let fetcher = FakeFetcher::new();
    fetcher.push(
        discovery_url(),
        Ok(discovery("https://issuer.example", jwks_url())),
    );
    fetcher.push(jwks_url(), Ok(verifying));
    let resolver = discovered_resolver(fetcher);
    let token = token("paired-subject", "https://issuer.example", "cortex", 300);
    assert!(resolver.resolve(&token).await.is_ok());

    for unusable in [
        serde_json::json!({"use":"enc"}),
        serde_json::json!({"key_ops":["sign"]}),
        serde_json::json!({"use":"sig","key_ops":["verify"]}),
    ] {
        let mut key = serde_json::json!({
            "kty":"OKP", "crv":"Ed25519",
            "x":"Gb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE",
            "alg":"EdDSA", "kid":"test-key"
        });
        key.as_object_mut()
            .expect("key object")
            .extend(unusable.as_object().expect("fields").clone());
        let fetcher = FakeFetcher::new();
        fetcher.push(
            discovery_url(),
            Ok(discovery("https://issuer.example", jwks_url())),
        );
        fetcher.push(
            jwks_url(),
            Ok(serde_json::to_vec(&serde_json::json!({"keys":[key]})).expect("fixture")),
        );
        let resolver = discovered_resolver(fetcher);
        assert_eq!(
            resolver.resolve(&token).await,
            Err(GatewayError::InvalidToken)
        );
    }
}

fn discovered_resolver(fetcher: FakeFetcher) -> PairedIdentityResolver {
    PairedIdentityResolver::from_discovery(
        metadata(),
        vec![OidcAlgorithm::EdDsa],
        vec![PairedSubject::new("paired-subject", PrincipalId::new()).expect("pairing")],
        Arc::new(fetcher),
        Duration::from_mins(1),
    )
    .expect("resolver")
}

fn discovery_url() -> &'static str {
    "https://issuer.example/.well-known/openid-configuration"
}

fn jwks_url() -> &'static str {
    "https://issuer.example/jwks"
}

fn discovery(issuer: &str, jwks_uri: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"issuer":issuer,"jwks_uri":jwks_uri})).expect("fixture")
}

fn jwks(kid: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"keys":[{
        "kty":"OKP", "crv":"Ed25519",
        "x":"Gb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE",
        "alg":"EdDSA", "kid":kid, "use":"sig"
    }]}))
    .expect("fixture")
}

type FakeResult = Result<Vec<u8>, GatewayError>;

#[derive(Clone, Default)]
struct FakeFetcher {
    responses: Arc<Mutex<HashMap<String, VecDeque<FakeResult>>>>,
    calls: Arc<AtomicUsize>,
}

impl FakeFetcher {
    fn new() -> Self {
        Self::default()
    }

    fn push(&self, url: &str, response: FakeResult) {
        self.responses
            .lock()
            .expect("responses")
            .entry(url.to_owned())
            .or_default()
            .push_back(response);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl OidcDocumentFetcher for FakeFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = FakeResult> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .lock()
                .expect("responses")
                .get_mut(url)
                .and_then(VecDeque::pop_front)
                .ok_or(GatewayError::OidcUnavailable)??;
            if response.len() > max_bytes {
                return Err(GatewayError::InvalidConfiguration);
            }
            Ok(response)
        })
    }
}
