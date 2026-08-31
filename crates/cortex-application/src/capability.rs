/// A typed operation exposed by the shared Cortex application boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    NoteCreate,
    NoteUpdate,
    NoteDelete,
    NoteRestore,
    NoteSearch,
    TaskCreate,
    TaskComplete,
    TaskUpdate,
    TaskDelete,
    TaskRestore,
    TaskList,
    MemoryCreate,
    MemoryCorrect,
    MemoryDelete,
    MemoryRestore,
    MemorySearch,
    KnowledgeRetrieve,
}

/// Whether Cortex needs to retain audit evidence for a capability invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditClassification {
    None,
    Decision,
    Mutation,
}

/// Whether an operation ID is required to make a capability safe to retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Idempotency {
    NotApplicable,
    Required,
}

/// Adapter-safe facts about one application capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityMetadata {
    pub destructive: bool,
    pub required_grant: Capability,
    pub mcp_name: &'static str,
    pub mutates_state: bool,
    pub audit_classification: AuditClassification,
    pub idempotency: Idempotency,
    pub mcp_description: &'static str,
}

impl Capability {
    #[must_use]
    pub const fn metadata(self) -> CapabilityMetadata {
        match self {
            Self::NoteCreate => mutation(self, "cortex_note_create", "Create a note", false),
            Self::NoteUpdate => mutation(self, "cortex_note_update", "Update a note", false),
            Self::NoteDelete => mutation(self, "cortex_note_delete", "Delete a note", true),
            Self::NoteRestore => mutation(self, "cortex_note_restore", "Restore a note", false),
            Self::NoteSearch => query(self, "cortex_note_search", "Search notes"),
            Self::TaskCreate => mutation(self, "cortex_task_create", "Create a task", false),
            Self::TaskComplete => mutation(self, "cortex_task_complete", "Complete a task", false),
            Self::TaskUpdate => mutation(self, "cortex_task_update", "Update a task", false),
            Self::TaskDelete => mutation(self, "cortex_task_delete", "Delete a task", true),
            Self::TaskRestore => mutation(self, "cortex_task_restore", "Restore a task", false),
            Self::TaskList => query(self, "cortex_task_list", "List tasks"),
            Self::MemoryCreate => mutation(self, "cortex_memory_create", "Create a memory", false),
            Self::MemoryCorrect => {
                mutation(self, "cortex_memory_correct", "Correct a memory", false)
            }
            Self::MemoryDelete => mutation(self, "cortex_memory_delete", "Delete a memory", true),
            Self::MemoryRestore => {
                mutation(self, "cortex_memory_restore", "Restore a memory", false)
            }
            Self::MemorySearch => query(self, "cortex_memory_search", "Search memories"),
            Self::KnowledgeRetrieve => {
                query(self, "cortex_knowledge_retrieve", "Retrieve knowledge")
            }
        }
    }
}

const fn mutation(
    required_grant: Capability,
    mcp_name: &'static str,
    mcp_description: &'static str,
    destructive: bool,
) -> CapabilityMetadata {
    CapabilityMetadata {
        destructive,
        required_grant,
        mcp_name,
        mutates_state: true,
        audit_classification: AuditClassification::Mutation,
        idempotency: Idempotency::Required,
        mcp_description,
    }
}

const fn query(
    required_grant: Capability,
    mcp_name: &'static str,
    mcp_description: &'static str,
) -> CapabilityMetadata {
    CapabilityMetadata {
        destructive: false,
        required_grant,
        mcp_name,
        mutates_state: false,
        audit_classification: AuditClassification::Decision,
        idempotency: Idempotency::NotApplicable,
        mcp_description,
    }
}

/// The canonical enumeration of operations available to adapters and agents.
pub struct CapabilityCatalog;

impl CapabilityCatalog {
    #[must_use]
    pub const fn all() -> &'static [Capability] {
        &[
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
        ]
    }
}
