#![forbid(unsafe_code)]

mod capability;
mod command;
mod error;
mod query;
mod secrets;
mod service;

pub use capability::{
    AuditClassification, Capability, CapabilityCatalog, CapabilityMetadata, ContractDescriptor,
    Idempotency,
};
pub use command::{
    CommandContext, MemoryCorrectInput, MemoryCreateInput, MutationResult, NoteCreateInput,
    NoteUpdateInput, TaskCreateInput,
};
pub use cortex_domain::{PolicyDecision, PolicyDeny};
pub use error::ApplicationError;
pub use query::{
    AggregateChange, AtomicMutation, AtomicMutationPort, MemoryRepository, NoteRepository,
    OperationIdentity, OperationResultRepository, RecordedOperation, SourceRepository,
    TaskRepository,
};
pub use secrets::{SecretRef, SecretStore};
pub use service::{
    AgentCapabilityExecutor, ApplicationService, AuditPort, CapabilityGrant, CortexService,
    GrantPolicy, PolicyPort,
};
