use cortex_domain::{
    EntityId, MemoryAssertion, Note, OperationId, Revision, Source, Task, WorkspaceId,
};

use crate::{ApplicationError, MutationResult};

/// Application-owned persistence port for note aggregates.
#[allow(async_fn_in_trait)]
pub trait NoteRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError>;

    async fn insert(&self, note: Note) -> Result<Note, ApplicationError>;

    async fn replace(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
        note: Note,
    ) -> Result<Note, ApplicationError>;

    async fn delete(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;

    async fn restore(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;
}

/// Application-owned persistence port for task aggregates.
#[allow(async_fn_in_trait)]
pub trait TaskRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError>;

    async fn insert(&self, task: Task) -> Result<Task, ApplicationError>;

    async fn replace(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
        task: Task,
    ) -> Result<Task, ApplicationError>;

    async fn delete(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;

    async fn restore(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;
}

/// Application-owned persistence port for memory assertion aggregates.
#[allow(async_fn_in_trait)]
pub trait MemoryRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError>;

    async fn insert(&self, memory: MemoryAssertion) -> Result<MemoryAssertion, ApplicationError>;

    async fn replace(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
        memory: MemoryAssertion,
    ) -> Result<MemoryAssertion, ApplicationError>;

    async fn delete(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;

    async fn restore(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;
}

/// Application-owned persistence port for immutable provenance sources.
#[allow(async_fn_in_trait)]
pub trait SourceRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Source>, ApplicationError>;

    async fn insert(&self, source: Source) -> Result<Source, ApplicationError>;
}

/// Application-owned idempotency port for completed mutation operations.
#[allow(async_fn_in_trait)]
pub trait OperationRepository: Send + Sync {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<MutationResult>, ApplicationError>;

    async fn record(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        result: MutationResult,
    ) -> Result<(), ApplicationError>;
}
