#![forbid(unsafe_code)]

mod agent;
mod nim;
mod openai_compatible;
mod provider;
mod routing;
mod system_prompt;

pub use agent::{AgentLimits, AgentRunner, AuthorizedCapabilities};
pub use nim::{NimConfig, NimDiscovery, NimTransport, ReqwestNimTransport};
pub use openai_compatible::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiTransport, ProviderLimits,
    ReqwestOpenAiTransport, TransportError,
};
pub use provider::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};
pub use routing::{
    DiscoveredModel, ModelCapability, ModelCatalog, ModelId, ModelRouter, ProviderProfile,
    ProviderProfileId, RoleRoutingPolicy, RoutedModel,
};
pub use system_prompt::SystemPrompt;
