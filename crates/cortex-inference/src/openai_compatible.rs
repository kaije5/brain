use std::{net::IpAddr, time::Duration};

use cortex_application::{ApplicationError, Embedding, EmbeddingProvider, SecretRef};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, ToolCall};

const MAX_MODEL_NAME_BYTES: usize = 256;

/// Validated configuration for a loopback OpenAI-compatible chat endpoint.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleConfig {
    endpoint: reqwest::Url,
    model: String,
    secret_reference: Option<SecretRef>,
    timeout: Duration,
}

impl OpenAiCompatibleConfig {
    /// Validates a local endpoint, model identifier, opaque secret reference, and timeout.
    ///
    /// # Errors
    /// Returns a safe validation error for malformed or non-loopback configuration.
    pub fn new(
        endpoint: impl AsRef<str>,
        model: impl Into<String>,
        secret_reference: Option<SecretRef>,
        timeout: Duration,
    ) -> Result<Self, ApplicationError> {
        let endpoint = reqwest::Url::parse(endpoint.as_ref())
            .map_err(|_| ApplicationError::Validation { field: "endpoint" })?;
        let valid_scheme = matches!(endpoint.scheme(), "http" | "https");
        let valid_authority = endpoint.username().is_empty() && endpoint.password().is_none();
        let loopback = endpoint.host_str().is_some_and(is_loopback_host);
        if !valid_scheme || !valid_authority || !loopback {
            return Err(ApplicationError::Validation { field: "endpoint" });
        }

        let model = model.into();
        if model.trim().is_empty()
            || model.len() > MAX_MODEL_NAME_BYTES
            || model.chars().any(char::is_control)
        {
            return Err(ApplicationError::Validation { field: "model" });
        }
        if timeout.is_zero() {
            return Err(ApplicationError::Validation { field: "timeout" });
        }

        Ok(Self {
            endpoint,
            model,
            secret_reference,
            timeout,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    #[must_use]
    pub const fn secret_reference(&self) -> Option<&SecretRef> {
        self.secret_reference.as_ref()
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Safe transport failure categories used by the provider adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Timeout,
    Unavailable,
}

/// Injectable JSON transport boundary for deterministic adapter tests.
#[allow(async_fn_in_trait)]
pub trait OpenAiTransport: Send + Sync {
    async fn post_json(
        &self,
        endpoint: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError>;
}

/// Reusable Reqwest transport for a configured local model server.
#[derive(Clone, Debug)]
pub struct ReqwestOpenAiTransport {
    client: reqwest::Client,
}

impl Default for ReqwestOpenAiTransport {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl OpenAiTransport for ReqwestOpenAiTransport {
    async fn post_json(
        &self,
        endpoint: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        let response = self
            .client
            .post(endpoint)
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| classify_reqwest_error(&error))?
            .error_for_status()
            .map_err(|error| classify_reqwest_error(&error))?;
        response
            .json()
            .await
            .map_err(|error| classify_reqwest_error(&error))
    }
}

fn classify_reqwest_error(error: &reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Unavailable
    }
}

/// OpenAI-compatible inference adapter parameterized by its transport.
pub struct OpenAiCompatibleProvider<T = ReqwestOpenAiTransport> {
    config: OpenAiCompatibleConfig,
    transport: T,
}

impl OpenAiCompatibleProvider<ReqwestOpenAiTransport> {
    #[must_use]
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self {
            config,
            transport: ReqwestOpenAiTransport::default(),
        }
    }
}

impl<T> OpenAiCompatibleProvider<T> {
    #[must_use]
    pub const fn with_transport(config: OpenAiCompatibleConfig, transport: T) -> Self {
        Self { config, transport }
    }

    #[must_use]
    pub const fn config(&self) -> &OpenAiCompatibleConfig {
        &self.config
    }
}

impl<T> InferenceProvider for OpenAiCompatibleProvider<T>
where
    T: OpenAiTransport,
{
    async fn complete(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ApplicationError> {
        let body = encode_request(&self.config.model, request)?;
        let response = self
            .transport
            .post_json(self.config.endpoint(), body, self.config.timeout)
            .await
            .map_err(map_transport_error)?;
        decode_response(response)
    }
}

impl<T> EmbeddingProvider for OpenAiCompatibleProvider<T>
where
    T: OpenAiTransport,
{
    async fn embed(&self, text: &str) -> Result<Embedding, ApplicationError> {
        if text.trim().is_empty() {
            return Err(ApplicationError::Validation {
                field: "embedding_text",
            });
        }
        let response = self
            .transport
            .post_json(
                self.config.endpoint(),
                json!({
                    "model": self.config.model,
                    "input": text,
                }),
                self.config.timeout,
            )
            .await
            .map_err(map_transport_error)?;
        decode_embedding(&self.config.model, response)
    }
}

fn map_transport_error(error: TransportError) -> ApplicationError {
    match error {
        TransportError::Timeout => ApplicationError::InferenceTimeout,
        TransportError::Unavailable => ApplicationError::InferenceUnavailable,
    }
}

fn encode_request(model: &str, request: InferenceRequest) -> Result<Value, ApplicationError> {
    let messages = request
        .messages
        .into_iter()
        .map(encode_message)
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto"
    }))
}

fn encode_message(message: InferenceMessage) -> Result<Value, ApplicationError> {
    match message {
        InferenceMessage::User { content } => Ok(json!({
            "role": "user",
            "content": content
        })),
        InferenceMessage::Assistant {
            content,
            tool_calls,
        } => Ok(json!({
            "role": "assistant",
            "content": content,
            "tool_calls": tool_calls.into_iter().map(encode_tool_call).collect::<Vec<_>>()
        })),
        InferenceMessage::Tool { call_id, content } => {
            let content =
                serde_json::to_string(&content).map_err(|_| ApplicationError::Internal)?;
            Ok(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content
            }))
        }
    }
}

fn encode_tool_call(call: ToolCall) -> Value {
    let ToolCall {
        id,
        name,
        arguments,
    } = call;
    json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments
        }
    })
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunctionCall,
}

#[derive(Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

fn decode_response(response: Value) -> Result<InferenceResponse, ApplicationError> {
    let response: OpenAiResponse =
        serde_json::from_value(response).map_err(|_| ApplicationError::MalformedModelOutput {
            reason: "invalid OpenAI-compatible response",
        })?;
    let choice =
        response
            .choices
            .into_iter()
            .next()
            .ok_or(ApplicationError::MalformedModelOutput {
                reason: "response contained no choices",
            })?;
    Ok(InferenceResponse {
        content: choice.message.content,
        tool_calls: choice
            .message
            .tool_calls
            .into_iter()
            .map(|call| ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: call.function.arguments,
            })
            .collect(),
    })
}

fn decode_embedding(model: &str, response: Value) -> Result<Embedding, ApplicationError> {
    let response: OpenAiEmbeddingResponse =
        serde_json::from_value(response).map_err(|_| ApplicationError::MalformedModelOutput {
            reason: "invalid OpenAI-compatible embedding response",
        })?;
    let values =
        response
            .data
            .into_iter()
            .next()
            .ok_or(ApplicationError::MalformedModelOutput {
                reason: "embedding response contained no data",
            })?;
    Embedding::new(model, model, values.embedding).map_err(|_| {
        ApplicationError::MalformedModelOutput {
            reason: "embedding response contained an invalid vector",
        }
    })
}
