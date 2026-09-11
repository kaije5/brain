#![forbid(unsafe_code)]

mod agent;
mod error;
mod nim;
mod openai_compatible;
mod provider;
mod routing;
mod system_prompt;

pub use agent::{AgentLimits, AgentRunner, AuthorizedCapabilities};
pub use error::{
    ProviderError, ProviderFailureCategory, classify_http_response, classify_network_error,
    map_provider_error, parse_retry_after, read_bounded_body, response_too_large,
};
pub use nim::{NimConfig, NimDiscovery, NimTransport, ReqwestNimTransport};
pub use openai_compatible::{
    OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiTransport, ProviderLimits,
    ReqwestOpenAiTransport,
};
pub use provider::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};
pub use routing::{
    ApiMode, AuthStrategy, DiscoveredModel, ModelCapability, ModelCatalog, ModelId, ModelRouter,
    ProfileTimeouts, ProviderProfile, ProviderProfileId, ProviderQuirks, RoleRoutingPolicy,
    RoutedModel,
};
pub use system_prompt::SystemPrompt;
