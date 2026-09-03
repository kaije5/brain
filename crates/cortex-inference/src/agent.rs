use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use cortex_application::{AgentCapabilityExecutor, ApplicationError, Capability, CommandContext};
use cortex_domain::OperationId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::{Uuid, Version};

use crate::{
    InferenceMessage, InferenceProvider, InferenceRequest, InferenceResponse, InferenceTool,
    ToolCall,
};

const MAX_PROMPT_BYTES: usize = 32 * 1024;
const MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TOOL_CALLS_PER_RESPONSE: usize = 32;
const MAX_TOOL_CALL_ID_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_CONFIGURED_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_CONFIGURED_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Hard limits applied to one local agent run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentLimits {
    pub max_iterations: u8,
    pub timeout: Duration,
    /// Maximum permitted occurrences of one call ID. A value of one rejects repeats.
    pub max_duplicate_calls: u8,
    pub max_message_bytes: usize,
    pub max_request_bytes: usize,
}

impl AgentLimits {
    /// Builds non-zero agent limits.
    ///
    /// # Errors
    /// Returns validation errors for limits that cannot permit useful bounded work.
    pub const fn new(
        max_iterations: u8,
        timeout: Duration,
        max_duplicate_calls: u8,
        max_message_bytes: usize,
        max_request_bytes: usize,
    ) -> Result<Self, ApplicationError> {
        if max_iterations == 0 {
            return Err(ApplicationError::Validation {
                field: "max_iterations",
            });
        }
        if timeout.is_zero() {
            return Err(ApplicationError::Validation { field: "timeout" });
        }
        if max_duplicate_calls == 0 {
            return Err(ApplicationError::Validation {
                field: "max_duplicate_calls",
            });
        }
        if max_message_bytes == 0 || max_message_bytes > MAX_CONFIGURED_MESSAGE_BYTES {
            return Err(ApplicationError::Validation {
                field: "max_message_bytes",
            });
        }
        if max_request_bytes == 0 || max_request_bytes > MAX_CONFIGURED_REQUEST_BYTES {
            return Err(ApplicationError::Validation {
                field: "max_request_bytes",
            });
        }
        Ok(Self {
            max_iterations,
            timeout,
            max_duplicate_calls,
            max_message_bytes,
            max_request_bytes,
        })
    }
}

/// The exact capability set authorized for one agent run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedCapabilities {
    capabilities: BTreeSet<Capability>,
}

impl AuthorizedCapabilities {
    /// Builds an authorization set while rejecting ambiguous duplicate grants.
    ///
    /// # Errors
    /// Returns a validation error when a capability occurs more than once.
    pub fn new(
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Result<Self, ApplicationError> {
        let mut validated = BTreeSet::new();
        for capability in capabilities {
            if !validated.insert(capability) {
                return Err(ApplicationError::Validation {
                    field: "allowed_capabilities",
                });
            }
        }
        Ok(Self {
            capabilities: validated,
        })
    }

    fn contains(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.capabilities.iter().copied()
    }
}

/// Bounded local agent loop using static dispatch for both native-async ports.
pub struct AgentRunner<P, S>
where
    P: InferenceProvider,
    S: AgentCapabilityExecutor,
{
    provider: Arc<P>,
    service: Arc<S>,
    limits: AgentLimits,
}

impl<P, S> AgentRunner<P, S>
where
    P: InferenceProvider,
    S: AgentCapabilityExecutor,
{
    #[must_use]
    pub const fn new(provider: Arc<P>, service: Arc<S>, limits: AgentLimits) -> Self {
        Self {
            provider,
            service,
            limits,
        }
    }

    /// Runs one bounded agent conversation for an authenticated principal.
    ///
    /// # Errors
    /// Returns typed inference, schema, policy, or application errors.
    pub async fn run(
        &self,
        context: CommandContext,
        prompt: &str,
        allowed_capabilities: AuthorizedCapabilities,
    ) -> Result<String, ApplicationError> {
        if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(ApplicationError::Validation { field: "prompt" });
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(self.limits.timeout)
            .ok_or(ApplicationError::Validation { field: "timeout" })?;
        self.run_inner(context, prompt, &allowed_capabilities, deadline)
            .await
    }

    async fn run_inner(
        &self,
        context: CommandContext,
        prompt: &str,
        allowed_capabilities: &AuthorizedCapabilities,
        deadline: tokio::time::Instant,
    ) -> Result<String, ApplicationError> {
        let tools: Vec<InferenceTool> = allowed_capabilities.iter().map(inference_tool).collect();
        let mut messages = vec![InferenceMessage::User {
            content: prompt.to_owned(),
        }];
        let mut call_states = BTreeMap::<String, CallState>::new();

        for _ in 0..self.limits.max_iterations {
            if tokio::time::Instant::now() >= deadline {
                return Err(ApplicationError::InferenceTimeout);
            }
            let request = InferenceRequest {
                messages: messages.clone(),
                tools: tools.clone(),
            };
            enforce_serialized_limit(
                &request,
                self.limits.max_request_bytes,
                "agent request exceeded configured byte limit",
            )?;
            let response = tokio::time::timeout_at(deadline, self.provider.complete(request))
                .await
                .map_err(|_| ApplicationError::InferenceTimeout)??;
            if response.tool_calls.is_empty() {
                return final_content(response, self.limits.max_message_bytes);
            }
            if response.tool_calls.len() > MAX_TOOL_CALLS_PER_RESPONSE {
                return malformed("too many tool calls in one response");
            }

            let assistant_message = InferenceMessage::Assistant {
                content: response.content,
                tool_calls: response.tool_calls.clone(),
            };
            enforce_serialized_limit(
                &assistant_message,
                self.limits.max_message_bytes,
                "assistant response exceeded configured byte limit",
            )?;
            for call in &response.tool_calls {
                validate_call_identity(call)?;
            }
            messages.push(assistant_message);
            for call in response.tool_calls {
                let capability = Capability::from_mcp_name(&call.name).ok_or(
                    ApplicationError::MalformedModelOutput {
                        reason: "unknown tool name",
                    },
                )?;
                if !allowed_capabilities.contains(capability) {
                    return malformed("tool is outside authorized capability set");
                }
                let payload = validated_arguments(capability, &call.arguments)?;
                let tool_context = next_tool_context(
                    context,
                    &call.id,
                    &mut call_states,
                    self.limits.max_duplicate_calls,
                )?;
                if tokio::time::Instant::now() >= deadline {
                    return Err(ApplicationError::InferenceTimeout);
                }
                let result = self
                    .service
                    .execute_agent_tool(tool_context, capability, payload)
                    .await?;
                let tool_message = InferenceMessage::Tool {
                    call_id: call.id,
                    content: result,
                };
                enforce_serialized_limit(
                    &tool_message,
                    self.limits.max_message_bytes,
                    "tool result exceeded configured byte limit",
                )?;
                messages.push(tool_message);
            }
        }
        malformed("agent iteration limit exceeded")
    }
}

#[derive(Clone, Copy)]
struct CallState {
    occurrences: u8,
    context: CommandContext,
}

fn next_tool_context(
    base: CommandContext,
    call_id: &str,
    states: &mut BTreeMap<String, CallState>,
    max_occurrences: u8,
) -> Result<CommandContext, ApplicationError> {
    if let Some(state) = states.get_mut(call_id) {
        if state.occurrences >= max_occurrences {
            return malformed("duplicate tool call ID exceeded configured occurrence limit");
        }
        state.occurrences = state.occurrences.saturating_add(1);
        return Ok(state.context);
    }
    let context = fresh_tool_context(base);
    states.insert(
        call_id.to_owned(),
        CallState {
            occurrences: 1,
            context,
        },
    );
    Ok(context)
}

fn fresh_tool_context(base: CommandContext) -> CommandContext {
    CommandContext::from_authenticated(
        base.workspace_id,
        base.principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn final_content(
    response: InferenceResponse,
    max_message_bytes: usize,
) -> Result<String, ApplicationError> {
    enforce_serialized_limit(
        &response,
        max_message_bytes,
        "assistant response exceeded configured byte limit",
    )?;
    response
        .content
        .filter(|content| !content.trim().is_empty() && content.len() <= MAX_TEXT_BYTES)
        .ok_or(ApplicationError::MalformedModelOutput {
            reason: "response contained neither valid text nor tool calls",
        })
}

fn enforce_serialized_limit<T: Serialize>(
    value: &T,
    limit: usize,
    reason: &'static str,
) -> Result<(), ApplicationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ApplicationError::Internal)?;
    if bytes.len() > limit {
        return malformed(reason);
    }
    Ok(())
}

fn validate_call_identity(call: &ToolCall) -> Result<(), ApplicationError> {
    if call.id.trim().is_empty()
        || call.id.len() > MAX_TOOL_CALL_ID_BYTES
        || call.name.trim().is_empty()
        || call.name.len() > MAX_TOOL_CALL_ID_BYTES
        || call.arguments.len() > MAX_TOOL_ARGUMENT_BYTES
    {
        return malformed("invalid tool call envelope");
    }
    Ok(())
}

fn malformed<T>(reason: &'static str) -> Result<T, ApplicationError> {
    Err(ApplicationError::MalformedModelOutput { reason })
}

fn inference_tool(capability: Capability) -> InferenceTool {
    let metadata = capability.metadata();
    InferenceTool {
        name: metadata.mcp_name.to_owned(),
        description: metadata.mcp_description.to_owned(),
        input_schema: input_schema(capability),
    }
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    let mut schema = json!({
        "type": "object",
        "required": required,
        "additionalProperties": false
    });
    schema["properties"] = properties;
    schema
}

fn input_schema(capability: Capability) -> Value {
    let entity_revision = json!({
        "entity_id": { "type": "string", "format": "uuid" },
        "expected_revision": { "type": "integer", "minimum": 1 }
    });
    match capability {
        Capability::NoteCreate => object_schema(
            &["title", "content"],
            json!({
                "title": { "type": "string", "minLength": 1 },
                "content": { "type": "string", "minLength": 1 }
            }),
        ),
        Capability::NoteUpdate => object_schema(
            &["entity_id", "expected_revision", "title", "content"],
            merge_properties(
                entity_revision,
                json!({
                    "title": { "type": "string", "minLength": 1 },
                    "content": { "type": "string", "minLength": 1 }
                }),
            ),
        ),
        Capability::NoteDelete
        | Capability::NoteRestore
        | Capability::TaskComplete
        | Capability::TaskDelete
        | Capability::TaskRestore
        | Capability::MemoryDelete
        | Capability::MemoryRestore => {
            object_schema(&["entity_id", "expected_revision"], entity_revision)
        }
        Capability::TaskCreate => object_schema(
            &["title"],
            json!({
                "title": { "type": "string", "minLength": 1 },
                "due_at": { "type": ["string", "null"], "format": "date-time" }
            }),
        ),
        Capability::TaskUpdate => object_schema(
            &["entity_id", "expected_revision"],
            merge_properties(
                entity_revision,
                json!({
                    "title": { "type": ["string", "null"], "minLength": 1 },
                    "due_at": { "type": ["string", "null"], "format": "date-time" }
                }),
            ),
        ),
        Capability::TaskList => object_schema(
            &[],
            json!({
                "include_completed": { "type": "boolean" },
                "limit": { "type": "integer", "minimum": 1 }
            }),
        ),
        Capability::MemoryCreate => object_schema(
            &[
                "statement",
                "normalized_subject",
                "normalized_predicate",
                "normalized_object",
                "sources",
            ],
            memory_properties(),
        ),
        Capability::MemoryCorrect => object_schema(
            &[
                "entity_id",
                "expected_revision",
                "statement",
                "normalized_subject",
                "normalized_predicate",
                "normalized_object",
                "sources",
            ],
            merge_properties(entity_revision, memory_properties()),
        ),
        Capability::NoteSearch | Capability::MemorySearch | Capability::KnowledgeRetrieve => {
            object_schema(
                &["query"],
                json!({
                    "query": { "type": "string", "minLength": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                }),
            )
        }
    }
}

fn memory_properties() -> Value {
    json!({
        "statement": { "type": "string", "minLength": 1 },
        "normalized_subject": { "type": "string", "minLength": 1 },
        "normalized_predicate": { "type": "string", "minLength": 1 },
        "normalized_object": { "type": "string", "minLength": 1 },
        "sources": {
            "type": "array",
            "minItems": 1,
            "items": {
                "type": "object",
                "required": ["source_id"],
                "properties": {
                    "source_id": { "type": "string", "format": "uuid" }
                },
                "additionalProperties": false
            }
        }
    })
}

fn merge_properties(mut left: Value, mut right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object_mut()) {
        left.append(right);
    }
    left
}

fn validated_arguments(capability: Capability, arguments: &str) -> Result<Value, ApplicationError> {
    let value: Value =
        serde_json::from_str(arguments).map_err(|_| ApplicationError::MalformedModelOutput {
            reason: "tool arguments are not valid JSON",
        })?;
    let normalized = match capability {
        Capability::NoteCreate => decode::<NoteCreateArguments>(value),
        Capability::NoteUpdate => decode::<NoteUpdateArguments>(value),
        Capability::NoteDelete
        | Capability::NoteRestore
        | Capability::TaskComplete
        | Capability::TaskDelete
        | Capability::TaskRestore
        | Capability::MemoryDelete
        | Capability::MemoryRestore => decode::<EntityRevisionArguments>(value),
        Capability::TaskCreate => decode::<TaskCreateArguments>(value),
        Capability::TaskUpdate => decode::<TaskUpdateArguments>(value),
        Capability::TaskList => decode::<TaskListArguments>(value),
        Capability::MemoryCreate => decode::<MemoryCreateArguments>(value),
        Capability::MemoryCorrect => decode::<MemoryCorrectArguments>(value),
        Capability::NoteSearch | Capability::MemorySearch | Capability::KnowledgeRetrieve => {
            decode::<SearchArguments>(value)
        }
    }?;
    validate_capability_semantics(capability, &normalized)?;
    validate_normalized_arguments(&normalized)?;
    Ok(normalized)
}

fn validate_capability_semantics(
    capability: Capability,
    value: &Value,
) -> Result<(), ApplicationError> {
    if matches!(
        capability,
        Capability::MemoryCreate | Capability::MemoryCorrect
    ) && value
        .get("sources")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return malformed("memory tool arguments require source evidence");
    }
    if capability == Capability::TaskUpdate
        && value.get("title").is_none_or(Value::is_null)
        && value.get("due_at").is_none()
    {
        return malformed("task update contains no change");
    }
    Ok(())
}

fn decode<T>(value: Value) -> Result<Value, ApplicationError>
where
    T: DeserializeOwned + Serialize,
{
    let typed: T =
        serde_json::from_value(value).map_err(|_| ApplicationError::MalformedModelOutput {
            reason: "tool arguments do not match capability schema",
        })?;
    serde_json::to_value(typed).map_err(|_| ApplicationError::Internal)
}

fn validate_normalized_arguments(value: &Value) -> Result<(), ApplicationError> {
    match value {
        Value::String(text) if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES => {
            malformed("tool string argument is blank or oversized")
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => Ok(()),
        Value::Array(values) => {
            if values.len() > MAX_TOOL_CALLS_PER_RESPONSE {
                return malformed("tool array argument is oversized");
            }
            for value in values {
                validate_normalized_arguments(value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_normalized_arguments(value)?;
            }
            Ok(())
        }
    }
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NoteCreateArguments {
    title: String,
    content: String,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NoteUpdateArguments {
    entity_id: V7Uuid,
    expected_revision: NonZeroU64,
    title: String,
    content: String,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntityRevisionArguments {
    entity_id: V7Uuid,
    expected_revision: NonZeroU64,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskCreateArguments {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    due_at: Option<DateTime<Utc>>,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskUpdateArguments {
    entity_id: V7Uuid,
    expected_revision: NonZeroU64,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "UpdateField::is_missing")]
    due_at: UpdateField<DateTime<Utc>>,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskListArguments {
    #[serde(skip_serializing_if = "Option::is_none")]
    include_completed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<NonZeroUsize>,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<NonZeroUsize>,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceArguments {
    source_id: V7Uuid,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MemoryCreateArguments {
    statement: String,
    normalized_subject: String,
    normalized_predicate: String,
    normalized_object: String,
    sources: Vec<SourceArguments>,
}

#[derive(serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MemoryCorrectArguments {
    entity_id: V7Uuid,
    expected_revision: NonZeroU64,
    statement: String,
    normalized_subject: String,
    normalized_predicate: String,
    normalized_object: String,
    sources: Vec<SourceArguments>,
}

#[derive(Clone, Copy, serde::Deserialize, Serialize)]
#[serde(try_from = "Uuid", into = "Uuid")]
struct V7Uuid(Uuid);

impl TryFrom<Uuid> for V7Uuid {
    type Error = &'static str;

    fn try_from(value: Uuid) -> Result<Self, Self::Error> {
        if value.get_version() == Some(Version::SortRand) {
            Ok(Self(value))
        } else {
            Err("identifier must be UUIDv7")
        }
    }
}

impl From<V7Uuid> for Uuid {
    fn from(value: V7Uuid) -> Self {
        value.0
    }
}

#[derive(Default)]
enum UpdateField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> UpdateField<T> {
    const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl<'de, T> Deserialize<'de> for UpdateField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

impl<T> Serialize for UpdateField<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Missing | Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}
