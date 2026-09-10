use std::{net::IpAddr, time::Duration};

use cortex_application::{ApplicationError, Embedding, EmbeddingProvider, SecretRef};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, ToolCall,
    error::{
        ProviderError, classify_http_response, classify_network_error, map_provider_error,
        read_bounded_body,
    },
};

const MAX_MODEL_NAME_BYTES: usize = 256;
pub(crate) const MAX_CONFIGURED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONFIGURED_EMBEDDING_INPUT_BYTES: usize = 1024 * 1024;
const MAX_CONFIGURED_EMBEDDING_DIMENSIONS: usize = 1024 * 1024;

/// Allocation limits enforced by the OpenAI-compatible boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderLimits {
    response_bytes: usize,
    embedding_input_bytes: usize,
    embedding_dimensions: usize,
}

impl ProviderLimits {
    /// Validates finite non-zero provider allocation limits.
    ///
    /// # Errors
    /// Returns a validation error when a limit is zero or exceeds the v0.1 ceiling.
    pub const fn new(
        max_response_bytes: usize,
        max_embedding_input_bytes: usize,
        max_embedding_dimensions: usize,
    ) -> Result<Self, ApplicationError> {
        if max_response_bytes == 0 || max_response_bytes > MAX_CONFIGURED_RESPONSE_BYTES {
            return Err(ApplicationError::Validation {
                field: "max_response_bytes",
            });
        }
        if max_embedding_input_bytes == 0
            || max_embedding_input_bytes > MAX_CONFIGURED_EMBEDDING_INPUT_BYTES
        {
            return Err(ApplicationError::Validation {
                field: "max_embedding_input_bytes",
            });
        }
        if max_embedding_dimensions == 0
            || max_embedding_dimensions > MAX_CONFIGURED_EMBEDDING_DIMENSIONS
        {
            return Err(ApplicationError::Validation {
                field: "max_embedding_dimensions",
            });
        }
        Ok(Self {
            response_bytes: max_response_bytes,
            embedding_input_bytes: max_embedding_input_bytes,
            embedding_dimensions: max_embedding_dimensions,
        })
    }
}

/// Validated configuration for loopback OpenAI-compatible operation routes.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleConfig {
    base_url: reqwest::Url,
    chat_endpoint: reqwest::Url,
    embedding_endpoint: reqwest::Url,
    model: String,
    secret_reference: Option<SecretRef>,
    timeout: Duration,
    limits: ProviderLimits,
}

impl OpenAiCompatibleConfig {
    /// Validates the endpoint, model identifier, opaque secret reference, and timeout.
    ///
    /// # Errors
    /// Returns a safe validation error for malformed endpoints. Loopback may
    /// use cleartext HTTP; any remote endpoint must use HTTPS, matching the
    /// NIM discovery adapter's transport boundary.
    pub fn new(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        secret_reference: Option<SecretRef>,
        timeout: Duration,
        limits: ProviderLimits,
    ) -> Result<Self, ApplicationError> {
        let mut base_url = reqwest::Url::parse(base_url.as_ref())
            .map_err(|_| ApplicationError::Validation { field: "endpoint" })?;
        let valid_scheme = matches!(base_url.scheme(), "http" | "https");
        let valid_authority = base_url.username().is_empty() && base_url.password().is_none();
        let loopback = base_url.host_str().is_some_and(is_loopback_host);
        let remote_https = base_url.scheme() == "https";
        if !valid_scheme
            || !valid_authority
            || !(loopback || remote_https)
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
        let chat_endpoint = base_url
            .join("chat/completions")
            .map_err(|_| ApplicationError::Validation { field: "endpoint" })?;
        let embedding_endpoint = base_url
            .join("embeddings")
            .map_err(|_| ApplicationError::Validation { field: "endpoint" })?;

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
            base_url,
            chat_endpoint,
            embedding_endpoint,
            model,
            secret_reference,
            timeout,
            limits,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    #[must_use]
    pub fn chat_endpoint(&self) -> &str {
        self.chat_endpoint.as_str()
    }

    #[must_use]
    pub fn embedding_endpoint(&self) -> &str {
        self.embedding_endpoint.as_str()
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

    #[must_use]
    pub const fn limits(&self) -> ProviderLimits {
        self.limits
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Injectable JSON transport boundary for deterministic adapter tests.
#[allow(async_fn_in_trait)]
pub trait OpenAiTransport: Send + Sync {
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError>;
}

/// Reusable Reqwest transport for a configured local model server.
#[derive(Clone, Debug)]
pub struct ReqwestOpenAiTransport {
    client: reqwest::Client,
}

impl Default for ReqwestOpenAiTransport {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            // Prompts must never leave the configured loopback endpoint: no ambient proxy may
            // observe them and no 307/308 redirect may replay the POST body elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("statically valid transport client configuration");
        Self { client }
    }
}

impl OpenAiTransport for ReqwestOpenAiTransport {
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let mut request = self.client.post(endpoint).timeout(timeout).json(&body);
        if let Some(value) = bearer.and_then(|token| {
            reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).ok()
        }) {
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| classify_network_error(&error))?;
        let status = response.status();
        // Only 4xx/5xx are provider failures; 3xx bodies still parse (redirects
        // are never followed, but their responses are not classified as errors).
        if status.is_client_error() || status.is_server_error() {
            // Provider error payloads are classified into typed categories;
            // headers other than `Retry-After` and the raw body are dropped.
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .cloned();
            let error_body = read_bounded_body(response, max_response_bytes).await?;
            return Err(classify_http_response(
                status.as_u16(),
                retry_after.as_ref(),
                &error_body,
            ));
        }
        read_bounded_body(response, max_response_bytes).await
    }
}

/// OpenAI-compatible inference adapter parameterized by its transport.
#[derive(Clone)]
pub struct OpenAiCompatibleProvider<T = ReqwestOpenAiTransport> {
    config: OpenAiCompatibleConfig,
    bearer: Option<String>,
    transport: T,
}

impl OpenAiCompatibleProvider<ReqwestOpenAiTransport> {
    #[must_use]
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self {
            config,
            bearer: None,
            transport: ReqwestOpenAiTransport::default(),
        }
    }
}

impl<T> OpenAiCompatibleProvider<T> {
    #[must_use]
    pub const fn with_transport(config: OpenAiCompatibleConfig, transport: T) -> Self {
        Self {
            config,
            bearer: None,
            transport,
        }
    }

    /// Attaches the resolved bearer credential for authenticated providers.
    /// The value is held only for the process lifetime and is never exposed
    /// through `Debug` or accessors.
    #[must_use]
    pub fn with_bearer(mut self, bearer: Option<String>) -> Self {
        self.bearer = bearer;
        self
    }

    #[must_use]
    pub const fn config(&self) -> &OpenAiCompatibleConfig {
        &self.config
    }

    fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
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
            .post_json(
                self.config.chat_endpoint(),
                self.bearer(),
                body,
                self.config.timeout,
                self.config.limits.response_bytes,
            )
            .await
            .map_err(|error| map_provider_error(&error))?;
        decode_response(&response)
    }
}

impl<T> EmbeddingProvider for OpenAiCompatibleProvider<T>
where
    T: OpenAiTransport,
{
    async fn embed(&self, text: &str) -> Result<Embedding, ApplicationError> {
        if text.trim().is_empty() || text.len() > self.config.limits.embedding_input_bytes {
            return Err(ApplicationError::Validation {
                field: "embedding_text",
            });
        }
        let response = self
            .transport
            .post_json(
                self.config.embedding_endpoint(),
                self.bearer(),
                json!({
                    "model": self.config.model,
                    "input": text,
                }),
                self.config.timeout,
                self.config.limits.response_bytes,
            )
            .await
            .map_err(|error| map_provider_error(&error))?;
        decode_embedding(
            &self.config.model,
            &response,
            self.config.limits.embedding_dimensions,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    use super::{OpenAiTransport, ReqwestOpenAiTransport};

    #[tokio::test]
    async fn redirects_are_not_followed_and_request_bodies_are_not_replayed() {
        let target = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("target listener");
        let redirector = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirector listener");
        let redirector_addr = redirector.local_addr().expect("redirector address");
        let target_addr = target.local_addr().expect("target address");

        let responder = tokio::spawn(async move {
            let (mut socket, _) = redirector.accept().await.expect("transport connects");
            let mut request = vec![0_u8; 4096];
            let received = socket.read(&mut request).await.expect("request bytes");
            request.truncate(received);
            let location = format!("http://{target_addr}/chat/completions");
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("redirect written");
            request
        });

        let transport = ReqwestOpenAiTransport::default();
        let body =
            json!({"model": "local", "messages": [{"role": "user", "content": "secret-prompt"}]});
        let result = transport
            .post_json(
                &format!("http://{redirector_addr}/chat/completions"),
                None,
                body,
                Duration::from_secs(5),
                1024 * 1024,
            )
            .await;

        let sent = responder.await.expect("redirector task");
        let sent = String::from_utf8(sent).expect("ASCII HTTP request");
        assert!(
            sent.contains("secret-prompt"),
            "body reached the loopback redirector"
        );
        // The 307 response is returned as-is rather than followed: its empty body is all the
        // transport sees, and the redirect target must never receive a connection.
        assert!(result.is_ok());
        let redirect_targeted = timeout(Duration::from_millis(300), target.accept()).await;
        assert!(
            redirect_targeted.is_err(),
            "redirect target must not receive the replayed request"
        );
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

fn decode_response(response: &[u8]) -> Result<InferenceResponse, ApplicationError> {
    let response: OpenAiResponse =
        serde_json::from_slice(response).map_err(|_| ApplicationError::MalformedModelOutput {
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

fn decode_embedding(
    model: &str,
    response: &[u8],
    max_dimensions: usize,
) -> Result<Embedding, ApplicationError> {
    let response: OpenAiEmbeddingResponse =
        serde_json::from_slice(response).map_err(|_| ApplicationError::MalformedModelOutput {
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
    if values.embedding.len() > max_dimensions {
        return Err(ApplicationError::MalformedModelOutput {
            reason: "embedding response exceeded configured dimension limit",
        });
    }
    Embedding::new(model, model, values.embedding).map_err(|_| {
        ApplicationError::MalformedModelOutput {
            reason: "embedding response contained an invalid vector",
        }
    })
}
