use uuid::Uuid;

use crate::{AuditEventId, EntityId, OperationId, PolicyDecision, PrincipalId, WorkspaceId};

/// The externally safe class of a completed or rejected operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditResult {
    Succeeded,
    Rejected,
    Failed,
}

/// Redacted evidence for an application operation.
///
/// Audit events deliberately carry identifiers, classifications, and policy
/// outcomes only. Callers must never place note content, memory text,
/// credentials, tool prompts, or storage diagnostics in this record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub workspace_id: WorkspaceId,
    pub principal_id: PrincipalId,
    pub operation_id: OperationId,
    pub correlation_id: Uuid,
    pub capability: &'static str,
    pub target_id: Option<EntityId>,
    pub policy_decision: PolicyDecision,
    pub result: AuditResult,
}
