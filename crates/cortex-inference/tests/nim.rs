use std::{sync::Mutex, time::Duration};

use cortex_application::{ApplicationError, SecretRef};
use cortex_inference::{
    DiscoveredModel, ModelCapability, ModelId, NimConfig, NimDiscovery, NimTransport,
    ProviderError, ProviderFailureCategory,
};

fn provider_error(category: ProviderFailureCategory) -> ProviderError {
    ProviderError::from_category(category)
}
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct FakeNimTransport {
    inner: std::sync::Arc<Mutex<FakeInner>>,
}

#[derive(Default)]
struct FakeInner {
    get_responses: Vec<Result<Vec<u8>, ProviderError>>,
    post_responses: Vec<Result<Vec<u8>, ProviderError>>,
    get_requests: Vec<(String, Option<String>)>,
    post_requests: Vec<(String, Option<String>, Value)>,
}

impl FakeNimTransport {
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake transport mutex")
    }
}

impl NimTransport for FakeNimTransport {
    async fn get_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let _ = (timeout, max_response_bytes);
        let mut inner = self.lock();
        inner
            .get_requests
            .push((endpoint.to_owned(), bearer.map(ToOwned::to_owned)));
        inner
            .get_responses
            .pop()
            .unwrap_or(Err(provider_error(ProviderFailureCategory::Unavailable)))
    }

    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let _ = (timeout, max_response_bytes);
        let mut inner = self.lock();
        inner
            .post_requests
            .push((endpoint.to_owned(), bearer.map(ToOwned::to_owned), body));
        inner
            .post_responses
            .pop()
            .unwrap_or(Err(provider_error(ProviderFailureCategory::Unavailable)))
    }
}

fn config() -> NimConfig {
    NimConfig::new(
        "https://integrate.api.nvidia.com",
        Some(SecretRef::new("nvidia/api-catalog/dev").expect("valid secret ref")),
        TIMEOUT,
    )
    .expect("valid NIM configuration")
}

fn transport(
    get_responses: Vec<Result<Vec<u8>, ProviderError>>,
    post_responses: Vec<Result<Vec<u8>, ProviderError>>,
) -> FakeNimTransport {
    FakeNimTransport {
        inner: std::sync::Arc::new(Mutex::new(FakeInner {
            get_responses,
            post_responses,
            get_requests: Vec::new(),
            post_requests: Vec::new(),
        })),
    }
}

fn models_page(ids: &[&str]) -> Vec<u8> {
    json!({
        "object": "list",
        "data": ids
            .iter()
            .map(|id| json!({"id": id, "object": "model"}))
            .collect::<Vec<_>>()
    })
    .to_string()
    .into_bytes()
}

fn chat_response(content: &str) -> Vec<u8> {
    json!({
        "choices": [{"message": {"role": "assistant", "content": content}}]
    })
    .to_string()
    .into_bytes()
}

#[test]
fn nim_config_rejects_remote_cleartext_endpoints() {
    let result = NimConfig::new("http://intel.example.com", None, TIMEOUT);
    assert!(matches!(result, Err(ApplicationError::Validation { .. })));
}

#[test]
fn nim_config_allows_https_and_loopback_http() {
    assert!(NimConfig::new("https://integrate.api.nvidia.com", None, TIMEOUT).is_ok());
    assert!(NimConfig::new("http://127.0.0.1:8000", None, TIMEOUT).is_ok());
}

#[tokio::test]
async fn versioned_base_urls_are_normalized_to_the_deployment_root() {
    let fake = transport(vec![Ok(models_page(&["zephyr-7b"]))], Vec::new());
    let config = NimConfig::new("https://integrate.api.nvidia.com/v1", None, TIMEOUT)
        .expect("versioned base is valid");
    let discovery = NimDiscovery::new(config, fake.clone());

    discovery.discover(None).await.expect("discovery succeeds");

    let requests = fake.lock().get_requests.clone();
    assert_eq!(
        requests[0].0, "https://integrate.api.nvidia.com/v1/models",
        "a /v1 base must not be joined into /v1/v1/models"
    );
}

#[tokio::test]
async fn discovery_hits_models_endpoint_and_normalizes_ids() {
    let fake = transport(
        vec![Ok(models_page(&[
            "meta/llama-3.1-70b-instruct",
            "zephyr-7b",
        ]))],
        Vec::new(),
    );
    let discovery = NimDiscovery::new(config(), fake.clone());

    let models = discovery
        .discover(Some("bearer-token"))
        .await
        .expect("discovery succeeds");

    let requests = fake.lock().get_requests.clone();
    let (endpoint, bearer) = &requests[0];
    assert_eq!(endpoint, "https://integrate.api.nvidia.com/v1/models");
    assert_eq!(bearer.as_deref(), Some("bearer-token"));
    let mut ids: Vec<&str> = models
        .iter()
        .map(|model| model.model_id().as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["meta/llama-3.1-70b-instruct", "zephyr-7b"]);
}

#[tokio::test]
async fn keyless_discovery_omits_the_authorization_header() {
    let fake = transport(vec![Ok(models_page(&["model-a"]))], Vec::new());
    let config = NimConfig::new("http://127.0.0.1:8000", None, TIMEOUT).expect("valid config");
    let discovery = NimDiscovery::new(config, fake.clone());

    discovery.discover(None).await.expect("keyless discovery");

    assert_eq!(fake.lock().get_requests[0].1, None);
}

#[tokio::test]
async fn discovery_maps_transport_failure_to_a_safe_typed_error() {
    let fake = transport(
        vec![Err(provider_error(ProviderFailureCategory::Unavailable))],
        Vec::new(),
    );
    let discovery = NimDiscovery::new(config(), fake.clone());

    let result = discovery.discover(Some("token")).await;

    assert!(matches!(
        result,
        Err(ApplicationError::InferenceUnavailable)
    ));
}

#[tokio::test]
async fn discovery_skips_entries_with_invalid_model_identifiers() {
    let fake = transport(
        vec![Ok(models_page(&["good-model", "bad\u{0}id"]))],
        Vec::new(),
    );
    let discovery = NimDiscovery::new(config(), fake.clone());

    let models = discovery.discover(None).await.expect("discovery succeeds");

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_id().as_str(), "good-model");
}

#[tokio::test]
async fn probing_records_only_capabilities_the_model_demonstrates() {
    // Tool probe succeeds; the structured-output probe is refused by the endpoint.
    let fake = transport(
        Vec::new(),
        vec![
            Err(provider_error(ProviderFailureCategory::Unavailable)),
            Ok(chat_response("ok")),
        ],
    );
    let discovery = NimDiscovery::new(config(), fake.clone());
    let model = DiscoveredModel::new(ModelId::new("meta/llama-3.1-70b-instruct").expect("id"));

    let probed = discovery
        .probe_model(model, Some("token"))
        .await
        .expect("probing completes");

    let now = chrono::Utc::now();
    assert!(probed.has_fresh_evidence(ModelCapability::ToolCalling, now, TIMEOUT));
    assert!(!probed.has_fresh_evidence(ModelCapability::StructuredOutput, now, TIMEOUT));
}

#[tokio::test]
async fn probe_payloads_stay_bounded_and_use_neutral_content() {
    let fake = transport(
        Vec::new(),
        vec![Ok(chat_response("ok")), Ok(chat_response("ok"))],
    );
    let discovery = NimDiscovery::new(config(), fake.clone());
    let model = DiscoveredModel::new(ModelId::new("m").expect("id"));

    discovery.probe_model(model, None).await.expect("probing");

    let requests = fake.lock().post_requests.clone();
    assert_eq!(requests.len(), 2);
    for (_, _, body) in &requests {
        let serialized = body.to_string();
        assert!(serialized.len() < 2 * 1024, "probe payloads stay small");
        assert!(!serialized.contains("personal"), "probe content is neutral");
        assert_eq!(body["max_tokens"], 1, "probes request minimal generation");
    }
}

#[tokio::test]
async fn refresh_discovers_then_probes_every_model_with_fresh_evidence() {
    let fake = transport(
        vec![Ok(models_page(&["model-a", "model-b"]))],
        vec![
            Ok(chat_response("ok")),
            Ok(chat_response("ok")),
            Ok(chat_response("ok")),
            Ok(chat_response("ok")),
        ],
    );
    let discovery = NimDiscovery::new(config(), fake.clone());

    let catalog = discovery.refresh(Some("token")).await.expect("refresh");

    assert_eq!(fake.lock().post_requests.len(), 4, "two probes per model");
    assert_eq!(catalog.models().len(), 2);
    let now = chrono::Utc::now();
    for model in catalog.models() {
        assert!(
            model.has_fresh_evidence(ModelCapability::ToolCalling, now, TIMEOUT)
                && model.has_fresh_evidence(ModelCapability::StructuredOutput, now, TIMEOUT),
            "fully probed models are eligible for agent turns"
        );
    }
}

#[tokio::test]
async fn refresh_failure_reports_a_degraded_state_without_fabricating_evidence() {
    let fake = transport(
        vec![Err(provider_error(ProviderFailureCategory::Timeout))],
        Vec::new(),
    );
    let discovery = NimDiscovery::new(config(), fake.clone());

    let result = discovery.refresh(Some("token")).await;

    assert!(
        matches!(result, Err(ApplicationError::InferenceTimeout)),
        "refresh failure must surface a typed degraded error"
    );
}

#[tokio::test]
async fn reqwest_transport_sends_bearer_credentials_and_rejects_redirects() {
    use cortex_inference::ReqwestNimTransport;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let responder = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("connection");
        let mut request = vec![0_u8; 4096];
        let received = socket.read(&mut request).await.expect("request bytes");
        request.truncate(received);
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response written");
        String::from_utf8(request).expect("ASCII request")
    });

    let transport = ReqwestNimTransport::default();
    let result = transport
        .get_json(
            &format!("http://{address}/v1/models"),
            Some("secret-bearer-value"),
            Duration::from_secs(5),
            1024 * 1024,
        )
        .await;

    let sent = responder.await.expect("responder task");
    assert!(result.is_ok(), "loopback GET succeeds: {result:?}");
    assert!(
        sent.to_ascii_lowercase()
            .contains("authorization: bearer secret-bearer-value")
    );
    assert!(sent.starts_with("GET /v1/models"));
}
