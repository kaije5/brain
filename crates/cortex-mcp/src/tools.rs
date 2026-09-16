use crate::McpError;
use chrono::{DateTime, Utc};
use cortex_application::Capability;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{collections::BTreeMap, num::NonZeroU64};
use uuid::Uuid;

const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_REVISION_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub destructive: bool,
    pub input_properties: BTreeMap<String, String>,
    pub input_schema: Value,
}

/// A normalized public tool: the stable `knowledge.*`/`task.*` name plus the
/// Cortex capability it dispatches as on the daemon boundary. Policy,
/// grants, and wire capabilities keep their capability identity — only the
/// MCP-facing contract is normalized (SCRUM-119; storage plan §11).
struct NormalizedTool {
    name: &'static str,
    capability: Capability,
}

const SUPPORTED: &[NormalizedTool] = &[
    NormalizedTool {
        name: "knowledge.create",
        capability: Capability::NoteCreate,
    },
    NormalizedTool {
        name: "knowledge.update",
        capability: Capability::NoteUpdate,
    },
    NormalizedTool {
        name: "knowledge.delete",
        capability: Capability::NoteDelete,
    },
    NormalizedTool {
        name: "knowledge.retrieve",
        capability: Capability::NoteSearch,
    },
    NormalizedTool {
        name: "task.create",
        capability: Capability::TaskCreate,
    },
    NormalizedTool {
        name: "task.update",
        capability: Capability::TaskUpdate,
    },
    NormalizedTool {
        name: "task.complete",
        capability: Capability::TaskComplete,
    },
    NormalizedTool {
        name: "task.delete",
        capability: Capability::TaskDelete,
    },
    NormalizedTool {
        name: "task.restore",
        capability: Capability::TaskRestore,
    },
    NormalizedTool {
        name: "task.list",
        capability: Capability::TaskList,
    },
    NormalizedTool {
        name: "memory.create",
        capability: Capability::MemoryCreate,
    },
    NormalizedTool {
        name: "memory.correct",
        capability: Capability::MemoryCorrect,
    },
    NormalizedTool {
        name: "memory.delete",
        capability: Capability::MemoryDelete,
    },
    NormalizedTool {
        name: "memory.restore",
        capability: Capability::MemoryRestore,
    },
    NormalizedTool {
        name: "memory.search",
        capability: Capability::MemorySearch,
    },
];

fn normalized(name: &str) -> Option<&'static NormalizedTool> {
    SUPPORTED.iter().find(|tool| tool.name == name)
}

/// The daemon wire capability a normalized tool dispatches as.
#[must_use]
pub fn wire_capability(name: &str) -> Option<Capability> {
    normalized(name).map(|tool| tool.capability)
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KnowledgeCreate {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    content: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KnowledgeUpdate {
    #[schemars(length(min = 1, max = MAX_RESOURCE_ID_BYTES))]
    resource_id: String,
    #[schemars(length(min = 1, max = MAX_REVISION_BYTES))]
    expected_revision: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    content: String,
}
/// A resource-scoped mutation gated on the opaque revision the caller
/// observed. Provider resource ids are opaque bounded strings, never
/// `SQLite` entity identity.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResourceCommand {
    #[schemars(length(min = 1, max = MAX_RESOURCE_ID_BYTES))]
    resource_id: String,
    #[schemars(length(min = 1, max = MAX_REVISION_BYTES))]
    expected_revision: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskCreate {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    due_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskUpdate {
    #[schemars(length(min = 1, max = MAX_RESOURCE_ID_BYTES))]
    resource_id: String,
    #[schemars(length(min = 1, max = MAX_REVISION_BYTES))]
    expected_revision: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    due_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskList {
    #[schemars(range(min = 1, max = 100))]
    limit: Option<usize>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    query: String,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<usize>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Source {
    source_id: Uuid,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Memory {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    statement: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_subject: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_predicate: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_object: String,
    #[schemars(length(min = 1, max = 64))]
    sources: Vec<Source>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryCorrect {
    entity_id: Uuid,
    expected_revision: NonZeroU64,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    statement: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_subject: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_predicate: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    normalized_object: String,
    #[schemars(length(min = 1, max = 64))]
    sources: Vec<Source>,
}

trait ValidToolInput {
    fn is_valid(&self) -> bool;
}

impl ValidToolInput for KnowledgeCreate {
    fn is_valid(&self) -> bool {
        valid_text(&self.title) && valid_text(&self.content)
    }
}

impl ValidToolInput for KnowledgeUpdate {
    fn is_valid(&self) -> bool {
        valid_resource_id(&self.resource_id)
            && valid_revision(&self.expected_revision)
            && valid_text(&self.title)
            && valid_text(&self.content)
    }
}

impl ValidToolInput for ResourceCommand {
    fn is_valid(&self) -> bool {
        valid_resource_id(&self.resource_id) && valid_revision(&self.expected_revision)
    }
}

impl ValidToolInput for Search {
    fn is_valid(&self) -> bool {
        valid_text(&self.query) && valid_limit(self.limit)
    }
}

impl ValidToolInput for TaskCreate {
    fn is_valid(&self) -> bool {
        valid_text(&self.title)
    }
}

impl ValidToolInput for TaskUpdate {
    fn is_valid(&self) -> bool {
        valid_resource_id(&self.resource_id)
            && valid_revision(&self.expected_revision)
            && valid_text(&self.title)
    }
}

impl ValidToolInput for TaskList {
    fn is_valid(&self) -> bool {
        valid_limit(self.limit)
    }
}

impl ValidToolInput for Source {
    fn is_valid(&self) -> bool {
        valid_entity_id(self.source_id)
    }
}

impl ValidToolInput for Memory {
    fn is_valid(&self) -> bool {
        valid_memory_fields(
            &self.statement,
            &self.normalized_subject,
            &self.normalized_predicate,
            &self.normalized_object,
            &self.sources,
        )
    }
}

impl ValidToolInput for MemoryCorrect {
    fn is_valid(&self) -> bool {
        valid_entity_id(self.entity_id)
            && valid_memory_fields(
                &self.statement,
                &self.normalized_subject,
                &self.normalized_predicate,
                &self.normalized_object,
                &self.sources,
            )
    }
}

impl ValidToolInput for MemoryEntity {
    fn is_valid(&self) -> bool {
        valid_entity_id(self.entity_id)
    }
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT_BYTES
}

fn valid_entity_id(value: Uuid) -> bool {
    value.get_version() == Some(uuid::Version::SortRand)
}

/// Provider resource ids are opaque bounded tokens without control
/// characters; they never carry workspace or principal identity.
fn valid_resource_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_RESOURCE_ID_BYTES
        && !value.chars().any(char::is_control)
}

/// Observed revisions are opaque bounded tokens.
fn valid_revision(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_REVISION_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_limit(value: Option<usize>) -> bool {
    value.is_none_or(|limit| (1..=100).contains(&limit))
}

fn valid_memory_fields(
    statement: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    sources: &[Source],
) -> bool {
    valid_text(statement)
        && valid_text(subject)
        && valid_text(predicate)
        && valid_text(object)
        && (1..=64).contains(&sources.len())
        && sources.iter().all(ValidToolInput::is_valid)
}
#[must_use]
pub fn tool_schemas() -> Vec<ToolSchema> {
    SUPPORTED.iter().map(|tool| schema(tool.name)).collect()
}
#[must_use]
pub fn tool_schema(name: &str) -> Option<ToolSchema> {
    normalized(name).map(|tool| schema(tool.name))
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemoryEntity {
    entity_id: Uuid,
    expected_revision: NonZeroU64,
}

/// Decodes and validates one tool call's arguments for a normalized tool.
///
/// Unknown tool names and payloads that violate the tool schema (unknown
/// fields, missing or oversized values) are rejected as invalid input.
///
/// # Errors
/// Returns a redacted invalid-input error for unknown tools or invalid
/// arguments.
pub fn decode_arguments(name: &str, value: Value) -> Result<Value, McpError> {
    normalized(name).ok_or_else(McpError::invalid_input)?;
    match name {
        "knowledge.create" => decode::<KnowledgeCreate>(value),
        "knowledge.update" => decode::<KnowledgeUpdate>(value),
        "knowledge.delete" | "task.complete" | "task.delete" | "task.restore" => {
            decode::<ResourceCommand>(value)
        }
        "knowledge.retrieve" | "memory.search" => decode::<Search>(value),
        "task.create" => decode::<TaskCreate>(value),
        "task.update" => decode::<TaskUpdate>(value),
        "task.list" => decode::<TaskList>(value),
        "memory.create" => decode::<Memory>(value),
        "memory.correct" => decode::<MemoryCorrect>(value),
        "memory.delete" | "memory.restore" => decode::<MemoryEntity>(value),
        _ => Err(McpError::invalid_input()),
    }
}

fn decode<T: DeserializeOwned + Serialize + ValidToolInput>(v: Value) -> Result<Value, McpError> {
    let decoded = serde_json::from_value::<T>(v).map_err(|_| McpError::invalid_input())?;
    if !decoded.is_valid() {
        return Err(McpError::invalid_input());
    }
    serde_json::to_value(decoded).map_err(|_| McpError::invalid_input())
}
fn schema(name: &str) -> ToolSchema {
    let tool = normalized(name).expect("normalized tool");
    let m = tool.capability.metadata();
    let input_schema = match tool.name {
        "knowledge.create" => js::<KnowledgeCreate>(),
        "knowledge.update" => js::<KnowledgeUpdate>(),
        "knowledge.delete" | "task.complete" | "task.delete" | "task.restore" => {
            js::<ResourceCommand>()
        }
        "knowledge.retrieve" | "memory.search" => js::<Search>(),
        "task.create" => js::<TaskCreate>(),
        "task.update" => js::<TaskUpdate>(),
        "task.list" => js::<TaskList>(),
        "memory.create" => js::<Memory>(),
        "memory.correct" => js::<MemoryCorrect>(),
        "memory.delete" | "memory.restore" => js::<MemoryEntity>(),
        _ => Value::Null,
    };
    let input_properties = input_schema
        .get("properties")
        .and_then(Value::as_object)
        .map_or_else(BTreeMap::new, |p| {
            p.iter()
                .map(|(n, v)| {
                    (
                        n.clone(),
                        v.get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("object")
                            .to_owned(),
                    )
                })
                .collect()
        });
    ToolSchema {
        name: tool.name.to_owned(),
        description: m.mcp_description.to_owned(),
        destructive: m.destructive,
        input_properties,
        input_schema,
    }
}
fn js<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or(Value::Null)
}
