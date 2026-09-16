p = 'crates/cortex-inference/src/openai_compatible.rs'
s = open(p, encoding='utf8').read()

# 1. Transport trait: default streaming method
old = '''    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: Value,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError>;
}'''
new = '''    async fn post_json(
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
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>;'''
assert old in s, 'transport trait'
s = s.replace(old, new)

# 2. Reqwest impl (post_json_stream) — insert after the reqwest post_json impl
old = '''            return Err(classify_http_response(
                status.as_u16(),
                retry_after.as_ref(),
                &error_body,
            ));
        }
        read_bounded_body(response, max_response_bytes).await
    }
}

/// OpenAI-compatible inference adapter parameterized by its transport.'''
new = '''            return Err(classify_http_response(
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

/// OpenAI-compatible inference adapter parameterized by its transport.'''
assert old in s, 'reqwest impl'
s = s.replace(old, new)

# 3. StreamAccumulator + complete_streaming override
old = '''impl<T> InferenceProvider for OpenAiCompatibleProvider<T>
where
    T: OpenAiTransport,
{'''
new = '''/// Accumulates OpenAI streaming deltas into the final response (SCRUM-80).
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
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                self.content.push_str(content);
                visible = Some(content.to_owned());
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let index =
                    call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let entry = self.tool_calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if entry.id.is_none() {
                        entry.id = Some(id.to_owned());
                    }
                }
                if let Some(name) = call
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                {
                    if entry.name.is_none() {
                        entry.name = Some(name.to_owned());
                    }
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
{'''
assert old in s, 'provider impl anchor'
s = s.replace(old, new)

# 4. complete_streaming override
old = '''    async fn complete(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ApplicationError> {
        let body = encode_request(&self.config.model, request, self.config.quirks)?;
        let (response, _report) = self
            .post_json_with_retries(self.config.chat_endpoint(), body)
            .await?;
        decode_response(&response)
    }
}'''
new = '''    async fn complete(
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
                match tokio::time::timeout(self.config.stale_stream_timeout(), stream.next())
                    .await
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
            decoder.feed(&bytes).map_err(|error| map_provider_error(&error))?;
            while let Some(event) =
                decoder.next_event().map_err(|error| map_provider_error(&error))?
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
}'''
assert old in s, 'complete override'
s = s.replace(old, new)

open(p, 'w', encoding='utf8', newline='\n').write(s)

# reqwest stream feature
p = 'crates/cortex-inference/Cargo.toml'
s = open(p, encoding='utf8').read()
s = s.replace('features = ["json", "rustls-tls"] }', 'features = ["json", "rustls-tls", "stream"] }')
open(p, 'w', encoding='utf8', newline='\n').write(s)
print('ok')
