#![forbid(unsafe_code)]

mod agent;
mod error;
mod openai_compatible;
mod openai_discovery;
mod provider;
mod retry;
mod routing;
mod system_prompt;

pub use agent::{AgentLimits, AgentRunner, AuthorizedCapabilities};
pub use error::{
    ProviderError, ProviderFailureCategory, classify_http_response, classify_network_error,
    map_provider_error, parse_retry_after, read_bounded_body, response_too_large,
};
pub use openai_compatible::{
    OpenAiApiBase, OpenAiCompatibleConfig, OpenAiCompatibleProvider, OpenAiTransport,
    ProviderLimits, ReqwestOpenAiTransport,
};
pub use openai_discovery::{OpenAiDiscoveryConfig, OpenAiModelDiscovery};
pub use provider::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};
pub use retry::{
    AttemptFailure, RetryAttempt, RetryClock, RetryJitter, RetryPolicy, RetryReport, RetrySleep,
    decorrelated_jitter_ms, run_with_default_policy, run_with_retries,
};
pub use routing::{
    ApiMode, AuthStrategy, DiscoveredModel, ModelCapability, ModelCatalog, ModelId, ModelRouter,
    ProfileTimeouts, ProviderProfile, ProviderProfileId, ProviderQuirks, RoleRoutingPolicy,
    RoutedModel,
};
pub use system_prompt::SystemPrompt;
