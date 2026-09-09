#![forbid(unsafe_code)]

mod agent;
mod openai_compatible;
mod provider;

pub use agent::{AgentLimits, AgentRunner, AuthorizedCapabilities};
pub use openai_compatible::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiTransport, ProviderLimits,
    ReqwestOpenAiTransport, TransportError,
};
pub use provider::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};
