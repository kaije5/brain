/// A typed operation exposed by the shared Cortex application boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Capability {
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
    KnowledgeCreate,
    KnowledgeUpdate,
    KnowledgeDelete,
    AgentRun,
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

/// A stable name for a capability boundary DTO or schema.
///
/// Concrete Rust DTOs and schema definitions are introduced by Task 6. These
/// names prevent transports from creating a second capability-to-schema map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContractDescriptor {
    pub name: &'static str,
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
    pub input_contract: ContractDescriptor,
    pub output_contract: ContractDescriptor,
}

macro_rules! mutation_metadata {
    ($capability:expr, $mcp_name:expr, $description:expr, $destructive:expr, $input:expr) => {
        CapabilityMetadata {
            destructive: $destructive,
            required_grant: $capability,
            mcp_name: $mcp_name,
            mutates_state: true,
            audit_classification: AuditClassification::Mutation,
            idempotency: Idempotency::Required,
            mcp_description: $description,
            input_contract: ContractDescriptor { name: $input },
            output_contract: ContractDescriptor {
                name: "MutationResult",
            },
        }
    };
}

macro_rules! query_metadata {
    ($capability:expr, $mcp_name:expr, $description:expr, $input:expr, $output:expr) => {
        CapabilityMetadata {
            destructive: false,
            required_grant: $capability,
            mcp_name: $mcp_name,
            mutates_state: false,
            audit_classification: AuditClassification::Decision,
            idempotency: Idempotency::NotApplicable,
            mcp_description: $description,
            input_contract: ContractDescriptor { name: $input },
            output_contract: ContractDescriptor { name: $output },
        }
    };
}

const TASK_CREATE: CapabilityMetadata = mutation_metadata!(
    Capability::TaskCreate,
    "cortex_task_create",
    "Create a task",
    false,
    "TaskCreateInput"
);
const TASK_COMPLETE: CapabilityMetadata = mutation_metadata!(
    Capability::TaskComplete,
    "cortex_task_complete",
    "Complete a task",
    false,
    "TaskCompleteInput"
);
const TASK_UPDATE: CapabilityMetadata = mutation_metadata!(
    Capability::TaskUpdate,
    "cortex_task_update",
    "Update a task",
    false,
    "TaskUpdateInput"
);
const TASK_DELETE: CapabilityMetadata = mutation_metadata!(
    Capability::TaskDelete,
    "cortex_task_delete",
    "Delete a task",
    true,
    "TaskDeleteInput"
);
const TASK_RESTORE: CapabilityMetadata = mutation_metadata!(
    Capability::TaskRestore,
    "cortex_task_restore",
    "Restore a task",
    false,
    "TaskRestoreInput"
);
const TASK_LIST: CapabilityMetadata = query_metadata!(
    Capability::TaskList,
    "cortex_task_list",
    "List tasks",
    "TaskListRequest",
    "TaskListResult"
);
const MEMORY_CREATE: CapabilityMetadata = mutation_metadata!(
    Capability::MemoryCreate,
    "cortex_memory_create",
    "Create a memory",
    false,
    "MemoryCreateInput"
);
const MEMORY_CORRECT: CapabilityMetadata = mutation_metadata!(
    Capability::MemoryCorrect,
    "cortex_memory_correct",
    "Correct a memory",
    false,
    "MemoryCorrectInput"
);
const MEMORY_DELETE: CapabilityMetadata = mutation_metadata!(
    Capability::MemoryDelete,
    "cortex_memory_delete",
    "Delete a memory",
    true,
    "MemoryDeleteInput"
);
const MEMORY_RESTORE: CapabilityMetadata = mutation_metadata!(
    Capability::MemoryRestore,
    "cortex_memory_restore",
    "Restore a memory",
    false,
    "MemoryRestoreInput"
);
const MEMORY_SEARCH: CapabilityMetadata = query_metadata!(
    Capability::MemorySearch,
    "cortex_memory_search",
    "Search memories",
    "MemorySearchRequest",
    "MemorySearchResultList"
);
const KNOWLEDGE_RETRIEVE: CapabilityMetadata = query_metadata!(
    Capability::KnowledgeRetrieve,
    "cortex_knowledge_search",
    "Search knowledge",
    "KnowledgeSearchRequest",
    "KnowledgeSearchResultList"
);
const KNOWLEDGE_CREATE: CapabilityMetadata = mutation_metadata!(
    Capability::KnowledgeCreate,
    "cortex_knowledge_create",
    "Create a knowledge document",
    false,
    "KnowledgeCreateInput"
);
const KNOWLEDGE_UPDATE: CapabilityMetadata = mutation_metadata!(
    Capability::KnowledgeUpdate,
    "cortex_knowledge_update",
    "Update a knowledge document",
    false,
    "KnowledgeUpdateInput"
);
const KNOWLEDGE_DELETE: CapabilityMetadata = mutation_metadata!(
    Capability::KnowledgeDelete,
    "cortex_knowledge_delete",
    "Delete a knowledge document",
    true,
    "KnowledgeDeleteInput"
);
const AGENT_RUN: CapabilityMetadata = query_metadata!(
    Capability::AgentRun,
    "cortex_agent_run",
    "Run the bounded local agent",
    "AgentRunRequest",
    "AgentRunResult"
);

impl Capability {
    #[must_use]
    pub const fn metadata(self) -> CapabilityMetadata {
        match self {
            Self::TaskCreate => TASK_CREATE,
            Self::TaskComplete => TASK_COMPLETE,
            Self::TaskUpdate => TASK_UPDATE,
            Self::TaskDelete => TASK_DELETE,
            Self::TaskRestore => TASK_RESTORE,
            Self::TaskList => TASK_LIST,
            Self::MemoryCreate => MEMORY_CREATE,
            Self::MemoryCorrect => MEMORY_CORRECT,
            Self::MemoryDelete => MEMORY_DELETE,
            Self::MemoryRestore => MEMORY_RESTORE,
            Self::MemorySearch => MEMORY_SEARCH,
            Self::KnowledgeRetrieve => KNOWLEDGE_RETRIEVE,
            Self::KnowledgeCreate => KNOWLEDGE_CREATE,
            Self::KnowledgeUpdate => KNOWLEDGE_UPDATE,
            Self::KnowledgeDelete => KNOWLEDGE_DELETE,
            Self::AgentRun => AGENT_RUN,
        }
    }

    #[must_use]
    pub fn from_mcp_name(name: &str) -> Option<Self> {
        CapabilityCatalog::all()
            .iter()
            .copied()
            .find(|capability| capability.metadata().mcp_name == name)
    }
}

/// The canonical enumeration of operations available to adapters and agents.
pub struct CapabilityCatalog;

impl CapabilityCatalog {
    #[must_use]
    pub const fn all() -> &'static [Capability] {
        &[
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
            Capability::KnowledgeCreate,
            Capability::KnowledgeUpdate,
            Capability::KnowledgeDelete,
            Capability::AgentRun,
        ]
    }
}
