use std::time::Duration;

use chrono::Utc;
use cortex_application::{ApplicationError, SecretRef};
use serde_json::{Value, json};

use crate::{
    DiscoveredModel, ModelCapability, ModelCatalog, ModelId, OpenAiApiBase, OpenAiTransport,
    ProviderFailureCategory, error::map_provider_error,
    openai_compatible::MAX_CONFIGURED_RESPONSE_BYTES,
};

const MAX_DISCOVERED_MODELS: usize = 128;
const PROBE_CONCURRENCY: usize = 8;
const MAX_DISCOVERY_RESPONSE_BYTES: usize = MAX_CONFIGURED_RESPONSE_BYTES;
const PROBE_MAX_TOKENS: u32 = 1;

/// Validated configuration for OpenAI-compatible model discovery and probes.
#[derive(Clone, Debug)]
pub struct OpenAiDiscoveryConfig {
    api_base: OpenAiApiBase,
    secret_reference: Option<SecretRef>,
    timeout: Duration,
}

impl OpenAiDiscoveryConfig {
    /// Validates the endpoint scheme and authority.
    ///
    /// # Errors
    /// Returns a validation error for malformed URLs or cleartext remote endpoints.
    pub fn new(
        base_url: impl AsRef<str>,
        secret_reference: Option<SecretRef>,
        timeout: Duration,
    ) -> Result<Self, ApplicationError> {
        let api_base = OpenAiApiBase::new(base_url)?;
        if timeout.is_zero() {
            return Err(ApplicationError::Validation { field: "timeout" });
        }
        Ok(Self {
            api_base,
            secret_reference,
            timeout,
        })
    }

    #[must_use]
    pub fn models_endpoint(&self) -> &str {
        self.api_base.models_endpoint()
    }

    #[must_use]
    pub fn chat_endpoint(&self) -> &str {
        self.api_base.chat_endpoint()
    }

    #[must_use]
    pub const fn secret_reference(&self) -> Option<&SecretRef> {
        self.secret_reference.as_ref()
    }
}

/// Provider-neutral model discovery through `GET /models` and per-model
/// OpenAI-compatible `POST /chat/completions` capability probes.
pub struct OpenAiModelDiscovery<T: OpenAiTransport> {
    config: OpenAiDiscoveryConfig,
    transport: T,
}

impl<T: OpenAiTransport> OpenAiModelDiscovery<T> {
    #[must_use]
    pub const fn new(config: OpenAiDiscoveryConfig, transport: T) -> Self {
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
                reason: "invalid OpenAI-compatible model list response",
            }
        })?;
        let entries = parsed.get("data").and_then(Value::as_array).ok_or(
            ApplicationError::MalformedModelOutput {
                reason: "OpenAI-compatible model list response contained no data array",
            },
        )?;
        if entries.len() > MAX_DISCOVERED_MODELS {
            return Err(ApplicationError::MalformedModelOutput {
                reason: "OpenAI-compatible model list exceeded the configured discovery budget",
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
            Ok(response) => Ok(if extra.is_null() {
                demonstrates_tool_calling(&response)
            } else {
                demonstrates_structured_output(&response)
            }),
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

fn demonstrates_tool_calling(response: &[u8]) -> bool {
    let Ok(value): Result<Value, _> = serde_json::from_slice(response) else {
        return false;
    };
    value["choices"][0]["message"]["tool_calls"]
        .as_array()
        .is_some_and(|calls| {
            calls.iter().any(|call| {
                call["function"]["name"] == "cortex_probe"
                    && call["function"]["arguments"]
                        .as_str()
                        .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
            })
        })
}

fn demonstrates_structured_output(response: &[u8]) -> bool {
    let Ok(value): Result<Value, _> = serde_json::from_slice(response) else {
        return false;
    };
    value["choices"][0]["message"]["content"]
        .as_str()
        .is_some_and(|content| serde_json::from_str::<Value>(content).is_ok())
}
