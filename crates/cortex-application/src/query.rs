use cortex_domain::{
    AuditEvent, EntityId, MemoryAssertion, Note, OperationId, Revision, Source, Task, WorkspaceId,
};

use crate::{ApplicationError, Capability, MutationResult};

/// Application-owned persistence port for note aggregates.
#[allow(async_fn_in_trait)]
pub trait NoteRepository: Send + Sync {
    /// Loads only an active note for ordinary query paths.
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError>;

    /// Explicit history access used by lifecycle commands and future history APIs.
    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError>;
}

/// Application-owned persistence port for task aggregates.
#[allow(async_fn_in_trait)]
pub trait TaskRepository: Send + Sync {
    /// Lists active tasks in a workspace, bounded by the adapter-provided page limit.
    async fn list_active(
        &self,
        workspace_id: WorkspaceId,
        limit: std::num::NonZeroUsize,
    ) -> Result<Vec<Task>, ApplicationError>;

    /// Loads only an active task for ordinary query paths.
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError>;

    /// Explicit history access used by lifecycle commands and future history APIs.
    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError>;
}

/// Application-owned persistence port for memory assertion aggregates.
#[allow(async_fn_in_trait)]
pub trait MemoryRepository: Send + Sync {
    /// Loads only an active, non-superseded memory for ordinary query paths.
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError>;

    /// Explicit history access used by correction/lifecycle commands and history APIs.
    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError>;
}

/// Application-owned persistence port for immutable provenance sources.
#[allow(async_fn_in_trait)]
pub trait SourceRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Source>, ApplicationError>;
}

/// Read side of operation idempotency.
///
/// This preflight lookup lets retries return their original result before
/// re-validating now-stale aggregate state. [`AtomicMutationPort`] remains the
/// authoritative race-safe lookup-and-record boundary.
#[allow(async_fn_in_trait)]
pub trait OperationResultRepository: Send + Sync {
    async fn find_result(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<RecordedOperation>, ApplicationError>;
}

/// Authenticated command identity retained with a durable idempotency result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationIdentity {
    pub principal_id: cortex_domain::PrincipalId,
    pub capability: Capability,
    pub target_id: Option<EntityId>,
}

impl OperationIdentity {
    #[must_use]
    pub const fn new(
        principal_id: cortex_domain::PrincipalId,
        capability: Capability,
        target_id: Option<EntityId>,
    ) -> Self {
        Self {
            principal_id,
            capability,
            target_id,
        }
    }
}

/// A replayable mutation result together with the command identity that created it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordedOperation {
    pub identity: OperationIdentity,
    pub result: MutationResult,
}

/// One aggregate change staged for a single atomic mutation transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregateChange {
    InsertNote(Note),
    ReplaceNote {
        entity_id: EntityId,
        expected_revision: Revision,
        note: Note,
    },
    DeleteNote {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    RestoreNote {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    InsertTask(Task),
    ReplaceTask {
        entity_id: EntityId,
        expected_revision: Revision,
        task: Task,
    },
    DeleteTask {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    RestoreTask {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    InsertMemory(MemoryAssertion),
    ReplaceMemory {
        entity_id: EntityId,
        expected_revision: Revision,
        memory: MemoryAssertion,
    },
    DeleteMemory {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    RestoreMemory {
        entity_id: EntityId,
        expected_revision: Revision,
    },
    InsertSource(Source),
    LinkMemorySource {
        memory_id: EntityId,
        source_id: EntityId,
    },
}

/// The complete state transition that must commit or roll back as one unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicMutation {
    pub workspace_id: WorkspaceId,
    pub operation_id: OperationId,
    pub identity: OperationIdentity,
    pub changes: Vec<AggregateChange>,
    pub result: MutationResult,
    pub audit_event: AuditEvent,
}

impl AtomicMutation {
    /// Builds a mutation only when its audit evidence identifies the same
    /// authenticated operation and its result target.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::Internal`] when the change set is empty or
    /// its audit evidence does not match the trusted command context/result.
    pub fn new(
        context: crate::CommandContext,
        capability: Capability,
        target_id: Option<EntityId>,
        changes: Vec<AggregateChange>,
        result: MutationResult,
        audit_event: AuditEvent,
    ) -> Result<Self, ApplicationError> {
        let matching_audit_context = audit_event.workspace_id == context.workspace_id
            && audit_event.principal_id == context.principal_id
            && audit_event.operation_id == context.operation_id
            && audit_event.correlation_id == context.correlation_id
            && audit_event.capability == capability.metadata().mcp_name
            && audit_event.target_id == Some(result.entity_id)
            && result.audit_correlation_id == context.correlation_id;

        if changes.is_empty() || !matching_audit_context {
            return Err(ApplicationError::Internal);
        }

        Ok(Self {
            workspace_id: context.workspace_id,
            operation_id: context.operation_id,
            identity: OperationIdentity::new(context.principal_id, capability, target_id),
            changes,
            result,
            audit_event,
        })
    }
}

/// Application-owned atomic mutation boundary for all state changes.
///
/// Implementations must first return the already-recorded result for the
/// workspace/operation pair. For a new operation they must apply every staged
/// aggregate change, append the included redacted audit event, and record the
/// supplied result in one storage transaction. Any failure rolls back all of
/// those effects. Application services must never split those responsibilities
/// across aggregate repositories or [`crate::AuditPort`].
#[allow(async_fn_in_trait)]
pub trait AtomicMutationPort: Send + Sync {
    async fn execute_once(
        &self,
        mutation: AtomicMutation,
    ) -> Result<MutationResult, ApplicationError>;
}
