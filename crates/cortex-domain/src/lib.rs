#![forbid(unsafe_code)]

mod audit;
mod entity;
mod error;
mod ids;
mod memory;
mod note;
mod policy;
mod provenance;
mod task;

pub use audit::{AuditEvent, AuditResult};
pub use entity::{Lifecycle, Revision};
pub use error::DomainError;
pub use ids::{AuditEventId, EntityId, OperationId, PrincipalId, WorkspaceId};
pub use memory::{ConflictSet, MemoryAssertion, MemoryAssertionInput, MemoryStatus};
pub use note::{Note, NoteInput};
pub use policy::{PolicyDecision, PolicyDeny};
pub use provenance::{Source, SourceInput, SourceRef};
pub use task::{Task, TaskInput, TaskStatus};

pub const WORKSPACE_ARCHITECTURE: &str = "modular-monolith";
pub const UNSAFE_CODE_FORBIDDEN: bool = true;
