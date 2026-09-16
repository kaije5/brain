use std::collections::BTreeMap;

use std::{net::IpAddr, time::Duration};

use cortex_application::{ApplicationError, Embedding, EmbeddingProvider, SecretRef};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse,
    ProviderFailureCategory, ToolCall,
    error::{
        ProviderError, classify_http_response, classify_network_error, map_provider_error,
        read_bounded_body,
    },
    retry::{AttemptFailure, RetryPolicy, RetryReport, run_with_default_policy},
    routing::{ProfileTimeouts, ProviderQuirks},
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

/// Validated OpenAI-compatible API base and derived resource endpoints.
///
/// The configured URL is the complete API base. Its path is preserved exactly;
/// this connector never adds or removes a version segment such as `/v1`.
#[derive(Clone, Debug)]
pub struct OpenAiApiBase {
    base_url: reqwest::Url,
    models_endpoint: reqwest::Url,
    chat_endpoint: reqwest::Url,
    embedding_endpoint: reqwest::Url,
}

impl OpenAiApiBase {
    /// Validates an API base and derives OpenAI-compatible resource endpoints.
    ///
    /// # Errors
    /// Returns a safe validation error for malformed endpoints. Loopback may
    /// use cleartext HTTP; any remote endpoint must use HTTPS.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, ApplicationError> {
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

        let endpoint = |path| {
            base_url
                .join(path)
                .map_err(|_| ApplicationError::Validation { field: "endpoint" })
        };
        Ok(Self {
            models_endpoint: endpoint("models")?,
            chat_endpoint: endpoint("chat/completions")?,
            embedding_endpoint: endpoint("embeddings")?,
            base_url,
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    #[must_use]
    pub fn models_endpoint(&self) -> &str {
        self.models_endpoint.as_str()
    }

    #[must_use]
    pub fn chat_endpoint(&self) -> &str {
        self.chat_endpoint.as_str()
    }

    #[must_use]
    pub fn embedding_endpoint(&self) -> &str {
        self.embedding_endpoint.as_str()
    }
}

/// Validated configuration for an OpenAI-compatible provider.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleConfig {
    api_base: OpenAiApiBase,
    model: String,
    secret_reference: Option<SecretRef>,
    timeout: Duration,
    /// Stale-stream watchdog for SSE streams: resets on every meaningful
    /// stream activity (SCRUM-80). Sourced from the provider profile when
    /// SCRUM-82 profile timeouts are configured.
    stale_stream_timeout: Duration,
    limits: ProviderLimits,
    quirks: ProviderQuirks,
}

impl OpenAiCompatibleConfig {
    /// Sets the stale-stream watchdog duration for SSE streams.
    #[must_use]
    pub fn with_stale_stream_timeout(mut self, stale_stream: Duration) -> Self {
        self.stale_stream_timeout = stale_stream;
        self
    }

    /// The stale-stream watchdog duration for SSE streams.
    #[must_use]
    pub const fn stale_stream_timeout(&self) -> Duration {
        self.stale_stream_timeout
    }

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
        let api_base = OpenAiApiBase::new(base_url)?;

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
            api_base,
            model,
            secret_reference,
            timeout,
            stale_stream_timeout: ProfileTimeouts::default().stale_stream(),
            limits,
            quirks: ProviderQuirks::default(),
        })
    }

    /// Attaches typed OpenAI-compatibility quirks from the provider profile.
    #[must_use]
    pub const fn with_quirks(mut self, quirks: ProviderQuirks) -> Self {
        self.quirks = quirks;
        self
    }

    #[must_use]
    pub const fn quirks(&self) -> ProviderQuirks {
        self.quirks
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        self.api_base.base_url()
    }

    #[must_use]
    pub fn chat_endpoint(&self) -> &str {
        self.api_base.chat_endpoint()
    }

    #[must_use]
    pub fn embedding_endpoint(&self) -> &str {
        self.api_base.embedding_endpoint()
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

    /// Sends a streaming chat-completion request and returns the raw byte
    /// stream of the response body (Server-Sent Events). Non-2xx responses
    /// are classified before any SSE parsing. The default implementation
    /// reports streaming as unsupported for transports that do not implement
    /// it (SCRUM-80).
    async fn post_json_stream(
        &self,
        _endpoint: &str,
        _bearer: Option<&str>,
        _body: Value,
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<ByteStream, ProviderError> {
        Err(ProviderError::from_category(
            ProviderFailureCategory::InvalidRequest,
        ))
    }
}

/// A byte stream of an in-flight provider response.
pub type ByteStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>;

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
    async fn get_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let mut request = self.client.get(endpoint).timeout(timeout);
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
        if status.is_client_error() || status.is_server_error() {
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

    async fn post_json_stream(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<ByteStream, ProviderError> {
        use futures::StreamExt;

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
        // Non-2xx responses are classified before any SSE parsing.
        if status.is_client_error() || status.is_server_error() {
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
        Ok(Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|error| classify_network_error(&error))
        })))
    }
}

/// OpenAI-compatible inference adapter parameterized by its transport.
#[derive(Clone)]
pub struct OpenAiCompatibleProvider<T = ReqwestOpenAiTransport> {
    config: OpenAiCompatibleConfig,
    bearer: Option<String>,
    transport: T,
    retry_policy: RetryPolicy,
}

impl OpenAiCompatibleProvider<ReqwestOpenAiTransport> {
    #[must_use]
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self {
            config,
            bearer: None,
            transport: ReqwestOpenAiTransport::default(),
            retry_policy: RetryPolicy::default(),
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
            retry_policy: RetryPolicy::DEFAULT,
        }
    }

    /// Overrides the bounded retry policy applied to transient transport
    /// failures. Every attempt targets the same resolved selection (ADR-024).
    #[must_use]
    pub const fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
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

    /// Runs one transport call under the bounded retry policy: retryable
    /// categories (per the SCRUM-84 taxonomy) retry the identical request on
    /// the same resolved selection with decorrelated-jitter backoff and
    /// `Retry-After` floors; everything else surfaces immediately.
    ///
    /// # Errors
    /// Returns the typed application error of the last attempt.
    async fn post_json_with_retries(
        &self,
        endpoint: &str,
        body: Value,
    ) -> Result<(Vec<u8>, RetryReport), ApplicationError>
    where
        T: OpenAiTransport,
    {
        run_with_default_policy(|_attempt| {
            let future = self.transport.post_json(
                endpoint,
                self.bearer(),
                body.clone(),
                self.config.timeout,
                self.config.limits.response_bytes,
            );
            async move { future.await.map_err(AttemptFailure::classified) }
        })
        .await
    }

    fn bearer(&self) -> Option<&str> {
        self.bearer.as_deref()
    }
}

/// Accumulates `OpenAI` streaming deltas into the final response (SCRUM-80).
/// Content deltas append in order; tool-call deltas accumulate by index with
/// id/name first-wins and argument fragments concatenated.
#[derive(Default)]
struct StreamAccumulator {
    content: String,
    tool_calls: BTreeMap<usize, StreamToolCall>,
}

#[derive(Default)]
struct StreamToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl StreamAccumulator {
    /// Feeds one `data:` payload. Returns the user-visible content delta, if
    /// any.
    ///
    /// # Errors
    /// Returns a classified malformed-response error for invalid JSON.
    fn feed_data(&mut self, data: &str) -> Result<Option<String>, ProviderError> {
        let payload: Value = serde_json::from_str(data).map_err(|_| {
            ProviderError::from_category(ProviderFailureCategory::MalformedResponse)
        })?;
        let Some(delta) = payload["choices"][0]["delta"].as_object() else {
            return Ok(None);
        };
        let mut visible = None;
        if let Some(content) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        {
            self.content.push_str(content);
            visible = Some(content.to_owned());
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                #[allow(clippy::cast_possible_truncation)] // provider indices are small ordinals
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let entry = self.tool_calls.entry(index).or_default();
                if entry.id.is_none() {
                    entry.id = call.get("id").and_then(Value::as_str).map(str::to_owned);
                }
                if entry.name.is_none() {
                    entry.name = call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    entry.arguments.push_str(arguments);
                }
            }
        }
        Ok(visible)
    }

    /// Builds the final response from the accumulated deltas.
    fn finish(self) -> InferenceResponse {
        let tool_calls = self
            .tool_calls
            .into_iter()
            .map(|(index, call)| ToolCall {
                id: call.id.unwrap_or_else(|| format!("stream-{index}")),
                name: call.name.unwrap_or_default(),
                arguments: call.arguments,
            })
            .collect();
        InferenceResponse {
            content: (!self.content.is_empty()).then_some(self.content),
            tool_calls,
        }
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
        let body = encode_request(&self.config.model, request, self.config.quirks)?;
        let (response, _report) = self
            .post_json_with_retries(self.config.chat_endpoint(), body)
            .await?;
        decode_response(&response)
    }

    /// True SSE streaming: sends `stream: true`, decodes deltas
    /// incrementally, enforces the stale-stream watchdog and byte caps, and
    /// forwards each user-visible content delta as it arrives (SCRUM-80).
    ///
    /// # Errors
    /// Returns typed timeout, unavailable, or malformed-response errors.
    async fn complete_streaming(
        &self,
        request: InferenceRequest,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<InferenceResponse, ApplicationError> {
        use futures::StreamExt;

        let mut body = encode_request(&self.config.model, request, self.config.quirks)?;
        body["stream"] = Value::Bool(true);
        let stream = self
            .transport
            .post_json_stream(
                self.config.chat_endpoint(),
                self.bearer(),
                body,
                self.config.timeout,
                self.config.limits.response_bytes,
            )
            .await
            .map_err(|error| map_provider_error(&error))?;
        let mut stream = std::pin::pin!(stream);

        let mut decoder = crate::sse::SseDecoder::new(self.config.limits.response_bytes);
        let mut accumulator = StreamAccumulator::default();
        let mut total_bytes = 0_usize;
        let mut done = false;
        loop {
            // The stale-stream watchdog resets on every chunk: a silent
            // stream fails as a typed timeout (SCRUM-80).
            let chunk =
                match tokio::time::timeout(self.config.stale_stream_timeout(), stream.next()).await
                {
                    Err(_elapsed) => return Err(ApplicationError::InferenceTimeout),
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk,
                };
            let bytes = chunk.map_err(|error| map_provider_error(&error))?;
            total_bytes += bytes.len();
            if total_bytes > self.config.limits.response_bytes {
                return Err(map_provider_error(&ProviderError::from_category(
                    ProviderFailureCategory::MalformedResponse,
                )));
            }
            decoder
                .feed(&bytes)
                .map_err(|error| map_provider_error(&error))?;
            while let Some(event) = decoder
                .next_event()
                .map_err(|error| map_provider_error(&error))?
            {
                match event {
                    crate::sse::SseEvent::Done => done = true,
                    crate::sse::SseEvent::Data(data) => {
                        let delta = accumulator
                            .feed_data(&data)
                            .map_err(|error| map_provider_error(&error))?;
                        if let Some(delta) = delta {
                            on_delta(&delta);
                        }
                    }
                }
            }
            if done {
                break;
            }
        }
        if !done {
            // EOF without the `[DONE]` terminator: a truncated stream.
            return Err(map_provider_error(&ProviderError::from_category(
                ProviderFailureCategory::MalformedResponse,
            )));
        }
        Ok(accumulator.finish())
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
        let (response, _report) = self
            .post_json_with_retries(
                self.config.embedding_endpoint(),
                json!({
                    "model": self.config.model,
                    "input": text,
                }),
            )
            .await?;
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

    #[tokio::test]
    async fn get_json_sends_a_bearer_token_to_the_configured_models_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let address = listener.local_addr().expect("listener address");
        let responder = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("transport connects");
            let mut request = vec![0_u8; 4096];
            let received = socket.read(&mut request).await.expect("request bytes");
            request.truncate(received);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":[]}",
                )
                .await
                .expect("response written");
            String::from_utf8(request).expect("ASCII HTTP request")
        });

        let response = ReqwestOpenAiTransport::default()
            .get_json(
                &format!("http://{address}/v1/models"),
                Some("provider-token"),
                Duration::from_secs(5),
                1024,
            )
            .await
            .expect("models response accepted");

        assert_eq!(response, br#"{"data":[]}"#);
        let request = responder.await.expect("responder task");
        assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Bearer provider-token"));
    }
}

fn encode_request(
    model: &str,
    request: InferenceRequest,
    quirks: ProviderQuirks,
) -> Result<Value, ApplicationError> {
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
    let mut body = json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto"
    });
    // The `omit_tool_choice` quirk (SCRUM-82): some OpenAI-compatible servers
    // reject `tool_choice` alongside `tools`; omitting the field leaves the
    // default (auto) selection in force.
    if quirks.omit_tool_choice() {
        body.as_object_mut()
            .expect("statically built object")
            .remove("tool_choice");
    }
    Ok(body)
}

fn encode_message(message: InferenceMessage) -> Result<Value, ApplicationError> {
    match message {
        InferenceMessage::System { content } => Ok(json!({
            "role": "system",
            "content": content
        })),
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
