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
pub use command::{CommandContext, MutationResult};
pub use cortex_domain::{PolicyDecision, PolicyDeny};
pub use error::ApplicationError;
pub use query::{
    AggregateChange, AtomicMutation, AtomicMutationPort, MemoryRepository, NoteRepository,
    SourceRepository, TaskRepository,
};
pub use secrets::{SecretRef, SecretStore};
pub use service::{
    AgentCapabilityExecutor, AuditPort, CapabilityGrant, CortexService, GrantPolicy, PolicyPort,
};
