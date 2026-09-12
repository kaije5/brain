use crate::McpError;
use chrono::{DateTime, Utc};
use cortex_application::Capability;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{collections::BTreeMap, num::NonZeroU64};
use uuid::{Uuid, Version};

const MAX_TEXT_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub destructive: bool,
    pub input_properties: BTreeMap<String, String>,
    pub input_schema: Value,
}
const SUPPORTED: &[Capability] = &[
    Capability::NoteCreate,
    Capability::NoteUpdate,
    Capability::NoteDelete,
    Capability::NoteRestore,
    Capability::NoteSearch,
    Capability::TaskCreate,
    Capability::TaskComplete,
    Capability::TaskUpdate,
    Capability::TaskDelete,
    Capability::TaskRestore,
    Capability::TaskList,
    Capability::MemoryCreate,
    Capability::MemoryCorrect,
    Capability::MemoryDelete,
    Capability::MemoryRestore,
    Capability::MemorySearch,
    Capability::KnowledgeRetrieve,
];
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteCreate {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    content: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteUpdate {
    entity_id: Uuid,
    expected_revision: NonZeroU64,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    content: String,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Entity {
    entity_id: Uuid,
    expected_revision: NonZeroU64,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KnowledgeTitleBody {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    body: String,
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
struct TaskCreate {
    #[schemars(length(min = 1, max = MAX_TEXT_BYTES))]
    title: String,
    due_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskUpdate {
    entity_id: Uuid,
    expected_revision: NonZeroU64,
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

impl ValidToolInput for NoteCreate {
    fn is_valid(&self) -> bool {
        valid_text(&self.title) && valid_text(&self.content)
    }
}

impl ValidToolInput for NoteUpdate {
    fn is_valid(&self) -> bool {
        valid_entity_id(self.entity_id) && valid_text(&self.title) && valid_text(&self.content)
    }
}
impl ValidToolInput for KnowledgeTitleBody {
    fn is_valid(&self) -> bool {
        valid_text(&self.title) && valid_text(&self.body)
    }
}

impl ValidToolInput for Entity {
    fn is_valid(&self) -> bool {
        valid_entity_id(self.entity_id)
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
        valid_entity_id(self.entity_id) && valid_text(&self.title)
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

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT_BYTES
}

fn valid_entity_id(value: Uuid) -> bool {
    value.get_version() == Some(Version::SortRand)
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
    SUPPORTED.iter().map(|c| schema(*c)).collect()
}
#[must_use]
pub fn tool_schema(name: &str) -> Option<ToolSchema> {
    SUPPORTED
        .iter()
        .copied()
        .find(|c| c.metadata().mcp_name == name)
        .map(schema)
}
pub fn decode_arguments(name: &str, value: Value) -> Result<Value, McpError> {
    let c = SUPPORTED
        .iter()
        .copied()
        .find(|c| c.metadata().mcp_name == name)
        .ok_or_else(McpError::invalid_input)?;
    match c {
        Capability::NoteCreate => decode::<NoteCreate>(value),
        Capability::KnowledgeCreate | Capability::KnowledgeUpdate => {
            decode::<KnowledgeTitleBody>(value)
        }
        Capability::NoteUpdate => decode::<NoteUpdate>(value),
        Capability::NoteDelete
        | Capability::KnowledgeDelete
        | Capability::NoteRestore
        | Capability::TaskComplete
        | Capability::TaskDelete
        | Capability::TaskRestore
        | Capability::MemoryDelete
        | Capability::MemoryRestore => decode::<Entity>(value),
        Capability::NoteSearch | Capability::MemorySearch | Capability::KnowledgeRetrieve => {
            decode::<Search>(value)
        }
        Capability::TaskCreate => decode::<TaskCreate>(value),
        Capability::TaskUpdate => decode::<TaskUpdate>(value),
        Capability::TaskList => decode::<TaskList>(value),
        Capability::MemoryCreate => decode::<Memory>(value),
        Capability::MemoryCorrect => decode::<MemoryCorrect>(value),
        Capability::AgentRun => Err(McpError::invalid_input()),
    }
}
fn decode<T: DeserializeOwned + Serialize + ValidToolInput>(v: Value) -> Result<Value, McpError> {
    let decoded = serde_json::from_value::<T>(v).map_err(|_| McpError::invalid_input())?;
    if !decoded.is_valid() {
        return Err(McpError::invalid_input());
    }
    serde_json::to_value(decoded).map_err(|_| McpError::invalid_input())
}
fn schema(c: Capability) -> ToolSchema {
    let m = c.metadata();
    let input_schema = match c {
        Capability::NoteCreate => js::<NoteCreate>(),
        Capability::KnowledgeCreate | Capability::KnowledgeUpdate => js::<KnowledgeTitleBody>(),
        Capability::NoteUpdate => js::<NoteUpdate>(),
        Capability::NoteDelete
        | Capability::KnowledgeDelete
        | Capability::NoteRestore
        | Capability::TaskComplete
        | Capability::TaskDelete
        | Capability::TaskRestore
        | Capability::MemoryDelete
        | Capability::MemoryRestore => js::<Entity>(),
        Capability::NoteSearch | Capability::MemorySearch | Capability::KnowledgeRetrieve => {
            js::<Search>()
        }
        Capability::TaskCreate => js::<TaskCreate>(),
        Capability::TaskUpdate => js::<TaskUpdate>(),
        Capability::TaskList => js::<TaskList>(),
        Capability::MemoryCreate => js::<Memory>(),
        Capability::MemoryCorrect => js::<MemoryCorrect>(),
        Capability::AgentRun => Value::Null,
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
        name: m.mcp_name.to_owned(),
        description: m.mcp_description.to_owned(),
        destructive: m.destructive,
        input_properties,
        input_schema,
    }
}
fn js<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or(Value::Null)
}
