#![forbid(unsafe_code)]

mod authority;
mod automation;
mod capability;
mod command;
mod error;
mod knowledge;
mod planning;
mod provider;
mod query;
mod review;
mod search;
mod secrets;
mod service;
mod task_provider;

pub use authority::{
    AuthorityPolicy, ProviderAuthority, ProviderMutationOutcome, ProviderOperationLog,
    ProviderOperationRecord,
};
pub use automation::{
    AutomationEngine, AutomationOutcome, AutomationRunLog, AutomationRunRecord, VaultChangeHandler,
    VaultChangeKind, VaultChangeTrigger,
};
pub use capability::{
    AuditClassification, Capability, CapabilityCatalog, CapabilityMetadata, ContractDescriptor,
    Idempotency,
};
pub use command::{
    CommandContext, MemoryCorrectInput, MemoryCreateInput, MutationResult,
};
pub use cortex_domain::{PolicyDecision, PolicyDeny};
pub use error::{ApplicationError, RecoveryHint};
pub use knowledge::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
    KnowledgeUpdate,
};
pub use planning::{
    DEFAULT_WORK_BLOCK_MINUTES, PlannedWorkBlock, PlanningService, PlanningWindow, WorkBlockIntent,
    WorkBlockLedger, WorkBlockLinkage, plan_schedule, work_block_intent,
};
pub use provider::{
    MAX_PROVIDER_RESULTS, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead,
};
pub use query::{
    AggregateChange, AtomicMutation, AtomicMutationPort, MemoryRepository, OperationIdentity,
    OperationResultRepository, RecordedOperation, SourceRepository,
};
pub use review::{ReviewRunKind, ReviewRunLog, ReviewRunRecord, ReviewService};
pub use search::{
    Embedding, EmbeddingProvider, EntityKind, IndexedVector, SearchCandidate, SearchIndex,
};
pub use secrets::{SecretRef, SecretStore};
pub use service::{
    AgentCapabilityExecutor, ApplicationService, AuditPort, CapabilityGrant, CortexService,
    GrantPolicy, PolicyPort,
};
pub use task_provider::{
    ProviderTask, ProviderTaskPriority, ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete,
    TaskProvider, TaskQuery, TaskSchedulingMetadata, TaskUpdate,
};
