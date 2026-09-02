use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use cortex_application::{
    AgentCapabilityExecutor, ApplicationError, Capability, CapabilityCatalog, CommandContext,
};
use cortex_domain::OperationId;
use serde::{Serialize, de::DeserializeOwned};
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

/// Hard limits applied to one local agent run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentLimits {
    pub max_iterations: u8,
    pub timeout: Duration,
    /// Maximum permitted occurrences of one call ID. A value of one rejects repeats.
    pub max_duplicate_calls: u8,
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
        Ok(Self {
            max_iterations,
            timeout,
            max_duplicate_calls,
        })
    }
}

/// Bounded local agent loop using static dispatch for both native-async ports.
pub struct AgentRunner<P, S>
where
    P: InferenceProvider,
    S: AgentCapabilityExecutor,
{
    provider: Arc<P>,
    catalog: CapabilityCatalog,
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
            catalog: CapabilityCatalog,
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
    ) -> Result<String, ApplicationError> {
        if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(ApplicationError::Validation { field: "prompt" });
        }
        tokio::time::timeout(self.limits.timeout, self.run_inner(context, prompt))
            .await
            .map_err(|_| ApplicationError::InferenceTimeout)?
    }

    async fn run_inner(
        &self,
        context: CommandContext,
        prompt: &str,
    ) -> Result<String, ApplicationError> {
        let tools: Vec<InferenceTool> = catalog_capabilities(&self.catalog)
            .iter()
            .copied()
            .map(inference_tool)
            .collect();
        let mut messages = vec![InferenceMessage::User {
            content: prompt.to_owned(),
        }];
        let mut seen_call_ids = BTreeSet::new();

        for _ in 0..self.limits.max_iterations {
            let response = self
                .provider
                .complete(InferenceRequest {
                    messages: messages.clone(),
                    tools: tools.clone(),
                })
                .await?;
            if response.tool_calls.is_empty() {
                return final_content(response);
            }
            if response.tool_calls.len() > MAX_TOOL_CALLS_PER_RESPONSE {
                return malformed("too many tool calls in one response");
            }

            messages.push(InferenceMessage::Assistant {
                content: response.content,
                tool_calls: response.tool_calls.clone(),
            });
            for call in response.tool_calls {
                validate_call_identity(&call)?;
                let repeated = seen_call_ids.contains(&call.id);
                let occurrences = u8::from(repeated).saturating_add(1);
                if repeated || occurrences > self.limits.max_duplicate_calls {
                    return malformed("duplicate tool call ID");
                }
                seen_call_ids.insert(call.id.clone());
                let capability = Capability::from_mcp_name(&call.name).ok_or(
                    ApplicationError::MalformedModelOutput {
                        reason: "unknown tool name",
                    },
                )?;
                let payload = validated_arguments(capability, &call.arguments)?;
                let result = self
                    .service
                    .execute_agent_tool(fresh_tool_context(context), capability, payload)
                    .await?;
                messages.push(InferenceMessage::Tool {
                    call_id: call.id,
                    content: result,
                });
            }
        }
        malformed("agent iteration limit exceeded")
    }
}

fn catalog_capabilities(_catalog: &CapabilityCatalog) -> &'static [Capability] {
    CapabilityCatalog::all()
}

fn fresh_tool_context(base: CommandContext) -> CommandContext {
    CommandContext::from_authenticated(
        base.workspace_id,
        base.principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn final_content(response: InferenceResponse) -> Result<String, ApplicationError> {
    response
        .content
        .filter(|content| !content.trim().is_empty() && content.len() <= MAX_TEXT_BYTES)
        .ok_or(ApplicationError::MalformedModelOutput {
            reason: "response contained neither valid text nor tool calls",
        })
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
        && value.get("due_at").is_none_or(Value::is_null)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    due_at: Option<DateTime<Utc>>,
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
