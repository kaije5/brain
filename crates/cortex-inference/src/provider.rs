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
}
