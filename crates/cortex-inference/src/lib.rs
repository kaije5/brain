#![forbid(unsafe_code)]

mod agent;
mod nim;
mod openai_compatible;
mod provider;
mod routing;

pub use agent::{AgentLimits, AgentRunner, AuthorizedCapabilities};
pub use openai_compatible::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiTransport, ProviderLimits,
    ReqwestOpenAiTransport, TransportError,
};
pub use nim::{NimConfig, NimDiscovery, NimTransport, ReqwestNimTransport};
pub use provider::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};
pub use routing::{
    DiscoveredModel, ModelCapability, ModelCatalog, ModelId, ModelRouter, ProviderProfile,
    ProviderProfileId, RoleRoutingPolicy, RoutedModel,
};
