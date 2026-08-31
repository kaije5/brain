use cortex_domain::{EntityId, Lifecycle, OperationId, PrincipalId, Revision, WorkspaceId};
use uuid::Uuid;

/// Authenticated command metadata supplied by Cortex-owned transport code.
///
/// MCP and CLI payloads must not contain a principal or workspace selection;
/// trusted adapters derive both identities before constructing this context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandContext {
    pub workspace_id: WorkspaceId,
    pub principal_id: PrincipalId,
    pub operation_id: OperationId,
    pub correlation_id: Uuid,
}

impl CommandContext {
    #[must_use]
    pub const fn from_authenticated(
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        operation_id: OperationId,
        correlation_id: Uuid,
    ) -> Self {
        Self {
            workspace_id,
            principal_id,
            operation_id,
            correlation_id,
        }
    }
}

/// Canonical evidence returned after a successful idempotent mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationResult {
    pub entity_id: EntityId,
    pub revision: Revision,
    pub lifecycle: Lifecycle,
    pub audit_correlation_id: Uuid,
}
