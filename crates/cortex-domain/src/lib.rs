#![forbid(unsafe_code)]

mod audit;
mod entity;
mod error;
mod ids;
mod memory;
mod policy;
mod provenance;
mod provider;
mod text;

pub use audit::{AuditEvent, AuditResult};
pub use entity::{Lifecycle, Revision};
pub use error::DomainError;
pub use ids::{AuditEventId, EntityId, OperationId, PrincipalId, TaskId, WorkspaceId};
pub use memory::{ConflictSet, MemoryAssertion, MemoryAssertionInput, MemoryStatus};
pub use policy::{PolicyDecision, PolicyDeny};
pub use provenance::{Source, SourceInput, SourceRef};
pub use provider::{
    ContentHash, ObservedRevision, ProviderAuditMetadata, ProviderId, ProviderProvenance,
    ProviderResourceId, ProviderResourceKind, ProviderResourceRef, ResourceTarget,
};

pub const WORKSPACE_ARCHITECTURE: &str = "modular-monolith";
pub const UNSAFE_CODE_FORBIDDEN: bool = true;
