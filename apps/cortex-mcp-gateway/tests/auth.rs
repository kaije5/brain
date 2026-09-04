use cortex_domain::PrincipalId;
use cortex_mcp_gateway::{
    BearerToken, GatewayError, OidcMetadata, PairedIdentityResolver, PairedSubject,
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
