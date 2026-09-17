use cortex_application::ApplicationError;
use serde::Serialize;
use serde_json::Value;

/// One provider-neutral function tool offered to an inference backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InferenceTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Provider-neutral conversation state retained by the bounded agent loop.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum InferenceMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        content: Value,
    },
}

/// A complete inference turn, including the only tools the model may request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InferenceRequest {
    pub messages: Vec<InferenceMessage>,
    pub tools: Vec<InferenceTool>,
}

/// One structured function request emitted by an inference backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Provider-neutral output for one inference turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InferenceResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

/// Native-async inference boundary. Consumers use static generic dispatch.
#[allow(async_fn_in_trait)]
pub trait InferenceProvider: Send + Sync {
    async fn complete(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ApplicationError>;

    /// True token-level streaming (SCRUM-80): forwards each user-visible
    /// content delta to `on_delta` as it arrives from the provider, before
    /// the full completion exists. The default implementation is the
    /// non-streaming completion with its final content emitted once —
    /// provider adapters override this with real SSE support.
    ///
    /// # Errors
    /// Returns the typed inference failure of the underlying provider.
    async fn complete_streaming(
        &self,
        request: InferenceRequest,
        on_delta: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<InferenceResponse, ApplicationError> {
        let response = self.complete(request).await?;
        if let Some(content) = response.content.as_deref().filter(|c| !c.is_empty()) {
            on_delta(content);
        }
        Ok(response)
    }
}
