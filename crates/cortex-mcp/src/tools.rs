use std::collections::BTreeMap;

use cortex_application::{Capability, CapabilityCatalog};

/// The adapter-owned view of a single MCP tool declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub destructive: bool,
    pub input_properties: BTreeMap<String, String>,
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

/// Returns only the v0.1 MCP capabilities, excluding daemon diagnostics and agent internals.
#[must_use]
pub fn tool_schemas() -> Vec<ToolSchema> {
    let _ = CapabilityCatalog::all();
    SUPPORTED
        .iter()
        .map(|capability| schema(*capability))
        .collect()
}

/// Finds a declared v0.1 MCP tool by its canonical application name.
#[must_use]
pub fn tool_schema(name: &str) -> Option<ToolSchema> {
    SUPPORTED
        .iter()
        .copied()
        .find(|capability| capability.metadata().mcp_name == name)
        .map(schema)
}

fn schema(capability: Capability) -> ToolSchema {
    let metadata = capability.metadata();
    ToolSchema {
        name: metadata.mcp_name.to_owned(),
        description: metadata.mcp_description.to_owned(),
        destructive: metadata.destructive,
        input_properties: properties(capability),
    }
}

fn properties(capability: Capability) -> BTreeMap<String, String> {
    let names: &[&str] = match capability {
        Capability::NoteCreate => &["title", "content"],
        Capability::NoteUpdate => &["entity_id", "expected_revision", "title", "content"],
        Capability::NoteDelete
        | Capability::NoteRestore
        | Capability::TaskComplete
        | Capability::TaskDelete
        | Capability::TaskRestore
        | Capability::MemoryDelete
        | Capability::MemoryRestore => &["entity_id", "expected_revision"],
        Capability::NoteSearch | Capability::MemorySearch | Capability::KnowledgeRetrieve => {
            &["query", "limit"]
        }
        Capability::TaskCreate => &["title", "due_at"],
        Capability::TaskUpdate => &["entity_id", "expected_revision", "title", "due_at"],
        Capability::TaskList => &["limit"],
        Capability::MemoryCreate => &[
            "statement",
            "normalized_subject",
            "normalized_predicate",
            "normalized_object",
            "source_ids",
        ],
        Capability::MemoryCorrect => &[
            "entity_id",
            "expected_revision",
            "statement",
            "normalized_subject",
            "normalized_predicate",
            "normalized_object",
            "source_ids",
        ],
        Capability::AgentRun => &[],
    };
    names
        .iter()
        .map(|name| ((*name).to_owned(), "string".to_owned()))
        .collect()
}
