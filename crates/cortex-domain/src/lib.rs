#![forbid(unsafe_code)]

mod entity;
mod error;
mod ids;

pub use entity::{Lifecycle, Revision};
pub use error::DomainError;
pub use ids::{AuditEventId, EntityId, OperationId, PrincipalId, WorkspaceId};

pub const WORKSPACE_ARCHITECTURE: &str = "modular-monolith";
pub const UNSAFE_CODE_FORBIDDEN: bool = true;
