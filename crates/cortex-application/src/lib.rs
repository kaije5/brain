#![forbid(unsafe_code)]

mod capability;
mod command;
mod error;
mod query;
mod service;

pub use capability::{
    AuditClassification, Capability, CapabilityCatalog, CapabilityMetadata, Idempotency,
};
pub use command::{CommandContext, MutationResult};
pub use cortex_domain::{PolicyDecision, PolicyDeny};
pub use error::ApplicationError;
pub use query::{
    MemoryRepository, NoteRepository, OperationRepository, SourceRepository, TaskRepository,
};
pub use service::{
    AgentCapabilityExecutor, AuditPort, CapabilityGrant, CortexService, GrantPolicy, PolicyPort,
};
