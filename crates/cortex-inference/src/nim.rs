use std::{net::IpAddr, time::Duration};

use chrono::Utc;
use cortex_application::{ApplicationError, SecretRef};
use serde_json::{Value, json};

use crate::{
    DiscoveredModel, ModelCapability, ModelCatalog, ModelId,
    error::{
        ProviderError, ProviderFailureCategory, classify_http_response, classify_network_error,
        map_provider_error, read_bounded_body,
    },
    openai_compatible::MAX_CONFIGURED_RESPONSE_BYTES,
};

const MAX_DISCOVERED_MODELS: usize = 128;
const PROBE_CONCURRENCY: usize = 8;
const MAX_DISCOVERY_RESPONSE_BYTES: usize = MAX_CONFIGURED_RESPONSE_BYTES;
const PROBE_MAX_TOKENS: u32 = 1;

/// Validated endpoint configuration for one NVIDIA NIM deployment.
///
/// Authentication is endpoint configuration, not a domain assumption: hosted
/// catalog deployments require a `SecretRef`, self-hosted deployments may be
/// keyless.
#[derive(Clone, Debug)]
pub struct NimConfig {
    base_url: reqwest::Url,
    secret_reference: Option<SecretRef>,
    timeout: Duration,
}

impl NimConfig {
    /// Validates the endpoint scheme and authority.
    ///
    /// # Errors
    /// Returns a validation error for malformed URLs or cleartext remote
    /// endpoints; loopback HTTP remains permitted for self-hosted NIM.
    pub fn new(
        base_url: impl AsRef<str>,
        secret_reference: Option<SecretRef>,
        timeout: Duration,
    ) -> Result<Self, ApplicationError> {
        let mut base_url = reqwest::Url::parse(base_url.as_ref())
            .map_err(|_| ApplicationError::Validation { field: "endpoint" })?;
        let valid_scheme = matches!(base_url.scheme(), "http" | "https");
        let valid_authority = base_url.username().is_empty() && base_url.password().is_none();
        let loopback = base_url.host_str().is_some_and(is_loopback_host);
        let cleartext_remote = base_url.scheme() == "http" && !loopback;
        if !valid_scheme
            || !valid_authority
            || cleartext_remote
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ApplicationError::Validation { field: "endpoint" });
        }
        if !base_url.path().ends_with('/') {
            let mut path = base_url.path().to_owned();
            path.push('/');
            base_url.set_path(&path);
        }
        // Accept both deployment-root (`https://host`) and versioned
        // (`https://host/v1`) bases: endpoint helpers join `v1/...` relative
        // to the root, so a trailing version segment is normalized away.
        if let Some(root) = base_url.path().strip_suffix("/v1/") {
            base_url.set_path(&format!("{root}/"));
        }
        if timeout.is_zero() {
            return Err(ApplicationError::Validation { field: "timeout" });
        }
        Ok(Self {
            base_url,
            secret_reference,
            timeout,
        })
    }

    fn models_endpoint(&self) -> String {
        self.base_url
            .join("v1/models")
            .expect("base path is joinable")
            .to_string()
    }

    fn chat_endpoint(&self) -> String {
        self.base_url
            .join("v1/chat/completions")
            .expect("base path is joinable")
            .to_string()
    }

    #[must_use]
    pub const fn secret_reference(&self) -> Option<&SecretRef> {
        self.secret_reference.as_ref()
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Transport boundary for the NIM adapter. Bearer credentials cross this
/// boundary only, are resolved from a `SecretRef` by the daemon, and are
/// never logged, traced, or retained by the adapter.
#[allow(async_fn_in_trait)]
pub trait NimTransport: Send + Sync {
    async fn get_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError>;

    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError>;
}

/// NVIDIA NIM adapter: automatic model discovery through `GET /v1/models`
/// plus per-model capability probing through the OpenAI-compatible
/// `POST /v1/chat/completions` contract. NVIDIA wire types never leave this
/// module; results are normalized [`DiscoveredModel`]s.
pub struct NimDiscovery<T: NimTransport> {
    config: NimConfig,
    transport: T,
}

impl<T: NimTransport> NimDiscovery<T> {
    #[must_use]
    pub const fn new(config: NimConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Discovers currently available models. Availability changes over time,
    /// so callers must treat every result as a fresh snapshot.
    ///
    /// # Errors
    /// Returns typed inference failures for transport, timeout, or malformed
    /// discovery payloads.
    pub async fn discover(
        &self,
        bearer: Option<&str>,
    ) -> Result<Vec<DiscoveredModel>, ApplicationError> {
        let response = self
            .transport
            .get_json(
                &self.config.models_endpoint(),
                bearer,
                self.config.timeout,
                MAX_DISCOVERY_RESPONSE_BYTES,
            )
            .await
            .map_err(|error| map_provider_error(&error))?;
        let parsed: Value = serde_json::from_slice(&response).map_err(|_| {
            ApplicationError::MalformedModelOutput {
                reason: "invalid NIM model list response",
            }
        })?;
        let entries = parsed.get("data").and_then(Value::as_array).ok_or(
            ApplicationError::MalformedModelOutput {
                reason: "NIM model list response contained no data array",
            },
        )?;
        if entries.len() > MAX_DISCOVERED_MODELS {
            return Err(ApplicationError::MalformedModelOutput {
                reason: "NIM model list exceeded the configured discovery budget",
            });
        }
        let mut models = Vec::new();
        for entry in entries {
            let Some(raw_id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Ok(model_id) = ModelId::new(raw_id) else {
                continue;
            };
            models.push(DiscoveredModel::new(model_id));
        }
        Ok(models)
    }

    /// Capability-probes one discovered model, recording evidence only for
    /// capabilities the model demonstrates. An unprobed capability is never
    /// assumed.
    ///
    /// # Errors
    /// Returns typed inference failures when a probe transport fails.
    pub async fn probe_model(
        &self,
        model: DiscoveredModel,
        bearer: Option<&str>,
    ) -> Result<DiscoveredModel, ApplicationError> {
        let observed_at = Utc::now();
        let model_name = model.model_id().as_str().to_owned();
        let mut probed = model;
        if self.probe_tool_calling(&model_name, bearer).await? {
            probed = probed.with_evidence(ModelCapability::ToolCalling, observed_at);
        }
        if self.probe_structured_output(&model_name, bearer).await? {
            probed = probed.with_evidence(ModelCapability::StructuredOutput, observed_at);
        }
        Ok(probed)
    }

    async fn probe(
        &self,
        model: &str,
        extra: Value,
        bearer: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": PROBE_MAX_TOKENS,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "cortex_probe",
                    "description": "Capability probe",
                    "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
                }
            }],
            "tool_choice": "auto",
        });
        let mut body = body;
        if let (Some(extra), Some(target)) = (extra.as_object(), body.as_object_mut()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        match self
            .transport
            .post_json(
                &self.config.chat_endpoint(),
                bearer,
                body,
                self.config.timeout,
                MAX_DISCOVERY_RESPONSE_BYTES,
            )
            .await
        {
            Ok(_) => Ok(true),
            // A rejected or over-budget probe means this model did not
            // demonstrate the capability; one slow model must not degrade
            // the whole catalog refresh. Credential and billing failures are
            // configuration defects and still abort the refresh.
            Err(error)
                if error.category().retryable()
                    || matches!(
                        error.category(),
                        ProviderFailureCategory::InvalidRequest
                            | ProviderFailureCategory::ContextOverflow
                            | ProviderFailureCategory::MalformedResponse
                    ) =>
            {
                Ok(false)
            }
            Err(error) => Err(map_provider_error(&error)),
        }
    }

    async fn probe_tool_calling(
        &self,
        model: &str,
        bearer: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        self.probe(model, json!(null), bearer).await
    }

    async fn probe_structured_output(
        &self,
        model: &str,
        bearer: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        self.probe(
            model,
            json!({"response_format": {"type": "json_object"}}),
            bearer,
        )
        .await
    }

    /// Refreshes the capability catalog: discovery followed by bounded
    /// probing of every discovered model. Probes run with bounded concurrency
    /// so large hosted catalogs do not stretch daemon startup; failures
    /// surface as typed errors so callers keep their previous catalog
    /// without fabricating evidence.
    ///
    /// # Errors
    /// Returns typed inference failures; never invents eligibility.
    pub async fn refresh(&self, bearer: Option<&str>) -> Result<ModelCatalog, ApplicationError> {
        let discovered = self.discover(bearer).await?;
        let mut probed = Vec::with_capacity(discovered.len());
        for chunk in discovered.chunks(PROBE_CONCURRENCY) {
            let mut results = futures::future::join_all(
                chunk
                    .iter()
                    .cloned()
                    .map(|model| self.probe_model(model, bearer)),
            )
            .await;
            for result in results.drain(..) {
                probed.push(result?);
            }
        }
        Ok(ModelCatalog::new(probed))
    }
}

/// Reqwest-based NIM transport. Prompts, credentials, and probe payloads
/// never traverse an ambient proxy and are never replayed by a redirect.
#[derive(Clone, Debug, Default)]
pub struct ReqwestNimTransport {
    client: std::sync::OnceLock<reqwest::Client>,
}

impl ReqwestNimTransport {
    fn client(&self) -> &reqwest::Client {
        self.client.get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("statically valid transport client configuration")
        })
    }

    fn auth_header(bearer: Option<&str>) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(value) = bearer.and_then(|token| {
            reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).ok()
        }) {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        headers
    }
}

/// Reads the body of a failed response and classifies it into the typed
/// provider error taxonomy.
async fn classify_failed_response(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> ProviderError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .cloned();
    // Over-budget error bodies classify as malformed responses rather than
    // masking the HTTP status.
    let body = read_bounded_body(response, max_response_bytes)
        .await
        .unwrap_or_default();
    classify_http_response(status, retry_after.as_ref(), &body)
}

impl NimTransport for ReqwestNimTransport {
    async fn get_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let response = self
            .client()
            .get(endpoint)
            .timeout(timeout)
            .headers(Self::auth_header(bearer))
            .send()
            .await
            .map_err(|error| classify_network_error(&error))?;
        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(classify_failed_response(response, max_response_bytes).await);
        }
        read_bounded_body(response, max_response_bytes).await
    }

    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let response = self
            .client()
            .post(endpoint)
            .timeout(timeout)
            .headers(Self::auth_header(bearer))
            .json(&body)
            .send()
            .await
            .map_err(|error| classify_network_error(&error))?;
        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(classify_failed_response(response, max_response_bytes).await);
        }
        read_bounded_body(response, max_response_bytes).await
    }
}
