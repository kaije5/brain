#![forbid(unsafe_code)]

mod capability;
mod command;
mod error;
mod knowledge;
mod provider;
mod query;
mod search;
mod secrets;
mod service;
mod task_provider;

pub use capability::{
    AuditClassification, Capability, CapabilityCatalog, CapabilityMetadata, ContractDescriptor,
    Idempotency,
};
pub use command::{
    CommandContext, MemoryCorrectInput, MemoryCreateInput, MutationResult, NoteCreateInput,
    NoteUpdateInput, TaskCreateInput, TaskUpdateInput,
};
pub use cortex_domain::{PolicyDecision, PolicyDeny};
pub use error::{ApplicationError, RecoveryHint};
pub use knowledge::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeQuery, KnowledgeUpdate,
};
pub use provider::{
    MAX_PROVIDER_RESULTS, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead,
};
pub use query::{
    AggregateChange, AtomicMutation, AtomicMutationPort, MemoryRepository, NoteRepository,
    OperationIdentity, OperationResultRepository, RecordedOperation, SourceRepository,
    TaskRepository,
};
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
    TaskQuery, TaskSchedulingMetadata, TaskUpdate,
};
