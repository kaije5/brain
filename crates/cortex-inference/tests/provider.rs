use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use cortex_application::{ApplicationError, EmbeddingProvider, SecretRef};
use cortex_inference::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceTool, OpenAiCompatibleConfig,
    OpenAiCompatibleProvider, OpenAiTransport, ToolCall, TransportError,
};
use serde_json::{Value, json};

#[derive(Clone)]
struct FakeTransport {
    response: Result<Value, TransportError>,
    requests: Arc<Mutex<Vec<(String, Value, Duration)>>>,
}

impl FakeTransport {
    fn returning(response: Result<Value, TransportError>) -> Self {
        Self {
            response,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().map_or(0, |requests| requests.len())
    }
}

impl OpenAiTransport for FakeTransport {
    async fn post_json(
        &self,
        endpoint: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        self.requests
            .lock()
            .map_err(|_| TransportError::Unavailable)?
            .push((endpoint.to_owned(), body, timeout));
        self.response.clone()
    }
}

fn config() -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig::new(
        "http://127.0.0.1:8000/v1/chat/completions",
        "nemotron-mini",
        Some(
            SecretRef::new("secret://local-model")
                .unwrap_or_else(|error| panic!("static secret reference must be valid: {error:?}")),
        ),
        Duration::from_secs(4),
    )
    .unwrap_or_else(|error| panic!("static provider config must be valid: {error:?}"))
}

fn request() -> InferenceRequest {
    InferenceRequest {
        messages: vec![InferenceMessage::User {
            content: "remember Cortex".to_owned(),
        }],
        tools: vec![InferenceTool {
            name: "cortex_note_create".to_owned(),
            description: "Create a note".to_owned(),
            input_schema: json!({
                "type": "object",
                "required": ["title", "content"],
                "properties": {
                    "title": { "type": "string" },
                    "content": { "type": "string" }
                },
                "additionalProperties": false
            }),
        }],
    }
}

#[tokio::test]
async fn adapter_maps_openai_tool_calls_into_provider_neutral_output() {
    let transport = FakeTransport::returning(Ok(json!({
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call-7",
                    "type": "function",
                    "function": {
                        "name": "cortex_note_create",
                        "arguments": "{\"title\":\"Cortex\",\"content\":\"local first\"}"
                    }
                }]
            }
        }]
    })));
    let provider = OpenAiCompatibleProvider::with_transport(config(), transport.clone());

    let response = provider.complete(request()).await;

    assert_eq!(
        response,
        Ok(cortex_inference::InferenceResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call-7".to_owned(),
                name: "cortex_note_create".to_owned(),
                arguments: "{\"title\":\"Cortex\",\"content\":\"local first\"}".to_owned(),
            }],
        })
    );
    assert_eq!(transport.request_count(), 1);
}

#[tokio::test]
async fn adapter_maps_transport_failures_to_safe_application_errors() {
    for (transport_error, expected) in [
        (TransportError::Timeout, ApplicationError::InferenceTimeout),
        (
            TransportError::Unavailable,
            ApplicationError::InferenceUnavailable,
        ),
    ] {
        let provider = OpenAiCompatibleProvider::with_transport(
            config(),
            FakeTransport::returning(Err(transport_error)),
        );

        assert_eq!(provider.complete(request()).await, Err(expected));
    }
}

#[tokio::test]
async fn malformed_openai_response_is_a_safe_typed_error() {
    let provider = OpenAiCompatibleProvider::with_transport(
        config(),
        FakeTransport::returning(Ok(json!({ "choices": [] }))),
    );

    assert!(matches!(
        provider.complete(request()).await,
        Err(ApplicationError::MalformedModelOutput { .. })
    ));
}

#[tokio::test]
async fn adapter_maps_openai_embeddings_into_the_application_port() {
    let transport = FakeTransport::returning(Ok(json!({
        "data": [{ "embedding": [0.25, -0.5, 0.75] }]
    })));
    let provider = OpenAiCompatibleProvider::with_transport(config(), transport.clone());

    let embedding = provider
        .embed("Cortex is local first")
        .await
        .unwrap_or_else(|error| panic!("fake embedding response must be accepted: {error:?}"));

    assert_eq!(embedding.model_id(), "nemotron-mini");
    assert_eq!(embedding.model_version(), "nemotron-mini");
    assert_eq!(embedding.values(), &[0.25, -0.5, 0.75]);
    assert_eq!(transport.request_count(), 1);
}

#[test]
fn invalid_provider_configuration_is_rejected_before_transport() {
    assert!(matches!(
        OpenAiCompatibleConfig::new(
            "file:///tmp/model",
            "nemotron-mini",
            None,
            Duration::from_secs(1),
        ),
        Err(ApplicationError::Validation { field: "endpoint" })
    ));
    assert!(matches!(
        OpenAiCompatibleConfig::new(
            "http://127.0.0.1:8000/v1/chat/completions",
            " ",
            None,
            Duration::from_secs(1),
        ),
        Err(ApplicationError::Validation { field: "model" })
    ));
}
