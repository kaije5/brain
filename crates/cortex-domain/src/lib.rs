#![forbid(unsafe_code)]

mod entity;
mod error;
mod ids;
mod memory;
mod note;
mod provenance;
mod task;

pub use entity::{Lifecycle, Revision};
pub use error::DomainError;
pub use ids::{AuditEventId, EntityId, OperationId, PrincipalId, WorkspaceId};
pub use memory::{ConflictSet, MemoryAssertion, MemoryAssertionInput, MemoryStatus};
pub use note::{Note, NoteInput};
pub use provenance::{Source, SourceInput, SourceRef};
pub use task::{Task, TaskInput, TaskStatus};

pub const WORKSPACE_ARCHITECTURE: &str = "modular-monolith";
pub const UNSAFE_CODE_FORBIDDEN: bool = true;
