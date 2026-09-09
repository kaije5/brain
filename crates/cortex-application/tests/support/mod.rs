#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use cortex_application::{
    AggregateChange, ApplicationError, ApplicationService, AtomicMutation, AtomicMutationPort,
    AuditPort, Capability, CapabilityGrant, CommandContext, GrantPolicy, MemoryRepository,
    MutationResult, NoteRepository, OperationResultRepository, RecordedOperation, SourceRepository,
    TaskRepository,
};
use cortex_domain::{
    AuditEvent, EntityId, Lifecycle, MemoryAssertion, MemoryStatus, Note, OperationId, PrincipalId,
    Revision, Source, SourceInput, Task, WorkspaceId,
};
use uuid::Uuid;

pub type TestService = ApplicationService<GrantPolicy, FakeState, FakeState, FakeState>;

#[derive(Clone, Default)]
pub struct FakeState {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    notes: BTreeMap<(WorkspaceId, EntityId), Note>,
    tasks: BTreeMap<(WorkspaceId, EntityId), Task>,
    memories: BTreeMap<(WorkspaceId, EntityId), MemoryAssertion>,
    sources: BTreeMap<(WorkspaceId, EntityId), Source>,
    operations: BTreeMap<(WorkspaceId, OperationId), RecordedOperation>,
    audits: Vec<AuditEvent>,
}

pub struct Fixture {
    pub service: TestService,
    pub state: FakeState,
    pub workspace_id: WorkspaceId,
    pub principal_id: PrincipalId,
}

impl Fixture {
    pub fn with_capabilities(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        let workspace_id = WorkspaceId::new();
        let principal_id = PrincipalId::new();
        let policy = GrantPolicy::new(
            capabilities
                .into_iter()
                .map(|capability| CapabilityGrant::new(workspace_id, principal_id, capability)),
        );
        let state = FakeState::default();
        let service = ApplicationService::new(policy, state.clone(), state.clone(), state.clone());
        Self {
            service,
            state,
            workspace_id,
            principal_id,
        }
    }

    pub fn all_mutations() -> Self {
        Self::with_capabilities([
            Capability::NoteCreate,
            Capability::NoteUpdate,
            Capability::NoteDelete,
            Capability::NoteRestore,
            Capability::TaskCreate,
            Capability::TaskUpdate,
            Capability::TaskComplete,
            Capability::TaskDelete,
            Capability::TaskRestore,
            Capability::MemoryCreate,
            Capability::MemoryCorrect,
            Capability::MemoryDelete,
            Capability::MemoryRestore,
        ])
    }

    pub fn context(&self) -> CommandContext {
        self.context_for(OperationId::new())
    }

    pub fn context_for(&self, operation_id: OperationId) -> CommandContext {
        CommandContext::from_authenticated(
            self.workspace_id,
            self.principal_id,
            operation_id,
            Uuid::now_v7(),
        )
    }

    pub fn seed_source(&self, reference: &str) -> Result<EntityId, String> {
        let source = Source::create(SourceInput {
            workspace_id: self.workspace_id,
            reference: reference.to_owned(),
        })
        .map_err(debug_error)?;
        let id = source.id();
        self.state
            .lock()?
            .sources
            .insert((self.workspace_id, id), source);
        Ok(id)
    }

    pub fn seed_deleted_source(&self, reference: &str) -> Result<EntityId, String> {
        let active = Source::create(SourceInput {
            workspace_id: self.workspace_id,
            reference: reference.to_owned(),
        })
        .map_err(debug_error)?;
        let source = Source::rehydrate(
            active.id(),
            self.workspace_id,
            active.reference().to_owned(),
            active.revision().next().map_err(debug_error)?,
            Lifecycle::Deleted,
        )
        .map_err(debug_error)?;
        let id = source.id();
        self.state
            .lock()?
            .sources
            .insert((self.workspace_id, id), source);
        Ok(id)
    }
}

impl FakeState {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, String> {
        self.inner
            .lock()
            .map_err(|_| "test state mutex poisoned".to_owned())
    }

    pub fn audits(&self) -> Result<Vec<AuditEvent>, String> {
        Ok(self.lock()?.audits.clone())
    }

    pub fn memory_count(&self) -> Result<usize, String> {
        Ok(self.lock()?.memories.len())
    }

    pub fn task_count(&self) -> Result<usize, String> {
        Ok(self.lock()?.tasks.len())
    }
}

impl NoteRepository for FakeState {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError> {
        Ok(NoteRepository::find_history(self, workspace_id, entity_id)
            .await?
            .filter(|note| note.lifecycle() == Lifecycle::Active))
    }

    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .notes
            .get(&(workspace_id, entity_id))
            .cloned())
    }
}

impl TaskRepository for FakeState {
    async fn list_active(
        &self,
        workspace_id: WorkspaceId,
        limit: std::num::NonZeroUsize,
    ) -> Result<Vec<Task>, ApplicationError> {
        let mut tasks: Vec<_> = self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .tasks
            .iter()
            .filter(|((stored_workspace, _), task)| {
                *stored_workspace == workspace_id && task.lifecycle() == Lifecycle::Active
            })
            .map(|(_, task)| task.clone())
            .collect();
        tasks.sort_by_key(Task::id);
        tasks.truncate(limit.get());
        Ok(tasks)
    }

    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError> {
        Ok(TaskRepository::find_history(self, workspace_id, entity_id)
            .await?
            .filter(|task| task.lifecycle() == Lifecycle::Active))
    }

    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .tasks
            .get(&(workspace_id, entity_id))
            .cloned())
    }
}

impl MemoryRepository for FakeState {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError> {
        Ok(
            MemoryRepository::find_history(self, workspace_id, entity_id)
                .await?
                .filter(|memory| {
                    memory.lifecycle() == Lifecycle::Active
                        && memory.status() == MemoryStatus::Active
                }),
        )
    }

    async fn find_history(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .memories
            .get(&(workspace_id, entity_id))
            .cloned())
    }
}

impl SourceRepository for FakeState {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Source>, ApplicationError> {
        // Deliberately expose history here to exercise the service's defensive lifecycle check.
        // The SQLite adapter has a separate contract test requiring active-only lookup.
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .sources
            .get(&(workspace_id, entity_id))
            .cloned())
    }
}

impl OperationResultRepository for FakeState {
    async fn find_result(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<RecordedOperation>, ApplicationError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .operations
            .get(&(workspace_id, operation_id))
            .copied())
    }
}

impl AuditPort for FakeState {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError> {
        self.inner
            .lock()
            .map_err(|_| ApplicationError::Internal)?
            .audits
            .push(event);
        Ok(())
    }
}

impl AtomicMutationPort for FakeState {
    async fn execute_once(
        &self,
        mutation: AtomicMutation,
    ) -> Result<MutationResult, ApplicationError> {
        let mut state = self.inner.lock().map_err(|_| ApplicationError::Internal)?;
        if let Some(recorded) = state
            .operations
            .get(&(mutation.workspace_id, mutation.operation_id))
        {
            return if recorded.identity == mutation.identity {
                Ok(recorded.result)
            } else {
                Err(ApplicationError::Conflict {
                    entity: "operation",
                })
            };
        }
        for change in mutation.changes {
            apply_change(&mut state, mutation.workspace_id, change)?;
        }
        state.audits.push(mutation.audit_event);
        state.operations.insert(
            (mutation.workspace_id, mutation.operation_id),
            RecordedOperation {
                identity: mutation.identity,
                result: mutation.result,
            },
        );
        Ok(mutation.result)
    }
}

fn apply_change(
    state: &mut State,
    workspace_id: WorkspaceId,
    change: AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        change @ (AggregateChange::InsertNote(_)
        | AggregateChange::ReplaceNote { .. }
        | AggregateChange::DeleteNote { .. }
        | AggregateChange::RestoreNote { .. }) => apply_note_change(state, workspace_id, change),
        change @ (AggregateChange::InsertTask(_)
        | AggregateChange::ReplaceTask { .. }
        | AggregateChange::DeleteTask { .. }
        | AggregateChange::RestoreTask { .. }) => apply_task_change(state, workspace_id, change),
        change @ (AggregateChange::InsertMemory(_)
        | AggregateChange::ReplaceMemory { .. }
        | AggregateChange::DeleteMemory { .. }
        | AggregateChange::RestoreMemory { .. }) => {
            apply_memory_change(state, workspace_id, change)
        }
        AggregateChange::InsertSource(source) => {
            state.sources.insert((workspace_id, source.id()), source);
            Ok(())
        }
        AggregateChange::LinkMemorySource { .. } => Ok(()),
    }
}

fn apply_note_change(
    state: &mut State,
    workspace_id: WorkspaceId,
    change: AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertNote(note) => {
            state.notes.insert((workspace_id, note.id()), note);
        }
        AggregateChange::ReplaceNote {
            entity_id,
            expected_revision,
            note,
        } => {
            require_revision(
                state
                    .notes
                    .get(&(workspace_id, entity_id))
                    .map(Note::revision),
                expected_revision,
                "note",
            )?;
            state.notes.insert((workspace_id, entity_id), note);
        }
        AggregateChange::DeleteNote {
            entity_id,
            expected_revision,
        } => {
            let note = state
                .notes
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "note" })?;
            require_revision(Some(note.revision()), expected_revision, "note")?;
            state.notes.insert(
                (workspace_id, entity_id),
                Note::rehydrate(
                    entity_id,
                    workspace_id,
                    note.title().to_owned(),
                    note.content().to_owned(),
                    expected_revision.next()?,
                    Lifecycle::Deleted,
                )?,
            );
        }
        AggregateChange::RestoreNote {
            entity_id,
            expected_revision,
        } => {
            let note = state
                .notes
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "note" })?;
            require_revision(Some(note.revision()), expected_revision, "note")?;
            state.notes.insert(
                (workspace_id, entity_id),
                Note::rehydrate(
                    entity_id,
                    workspace_id,
                    note.title().to_owned(),
                    note.content().to_owned(),
                    expected_revision.next()?,
                    Lifecycle::Active,
                )?,
            );
        }
        _ => return Err(ApplicationError::Internal),
    }
    Ok(())
}

fn apply_task_change(
    state: &mut State,
    workspace_id: WorkspaceId,
    change: AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertTask(task) => {
            state.tasks.insert((workspace_id, task.id()), task);
        }
        AggregateChange::ReplaceTask {
            entity_id,
            expected_revision,
            task,
        } => {
            require_revision(
                state
                    .tasks
                    .get(&(workspace_id, entity_id))
                    .map(Task::revision),
                expected_revision,
                "task",
            )?;
            state.tasks.insert((workspace_id, entity_id), task);
        }
        AggregateChange::DeleteTask {
            entity_id,
            expected_revision,
        } => {
            let task = state
                .tasks
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "task" })?;
            require_revision(Some(task.revision()), expected_revision, "task")?;
            state.tasks.insert(
                (workspace_id, entity_id),
                Task::rehydrate(
                    entity_id,
                    workspace_id,
                    task.title().to_owned(),
                    task.due_at(),
                    task.status(),
                    expected_revision.next()?,
                    Lifecycle::Deleted,
                )?,
            );
        }
        AggregateChange::RestoreTask {
            entity_id,
            expected_revision,
        } => {
            let task = state
                .tasks
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "task" })?;
            require_revision(Some(task.revision()), expected_revision, "task")?;
            state.tasks.insert(
                (workspace_id, entity_id),
                Task::rehydrate(
                    entity_id,
                    workspace_id,
                    task.title().to_owned(),
                    task.due_at(),
                    task.status(),
                    expected_revision.next()?,
                    Lifecycle::Active,
                )?,
            );
        }
        _ => return Err(ApplicationError::Internal),
    }
    Ok(())
}

fn apply_memory_change(
    state: &mut State,
    workspace_id: WorkspaceId,
    change: AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertMemory(memory) => {
            state.memories.insert((workspace_id, memory.id()), memory);
        }
        AggregateChange::ReplaceMemory {
            entity_id,
            expected_revision,
            memory,
        } => {
            require_revision(
                state
                    .memories
                    .get(&(workspace_id, entity_id))
                    .map(MemoryAssertion::revision),
                expected_revision,
                "memory",
            )?;
            state.memories.insert((workspace_id, entity_id), memory);
        }
        AggregateChange::DeleteMemory {
            entity_id,
            expected_revision,
        } => {
            let memory = state
                .memories
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "memory" })?;
            require_revision(Some(memory.revision()), expected_revision, "memory")?;
            state.memories.insert(
                (workspace_id, entity_id),
                rehydrate_memory(
                    &memory,
                    expected_revision.next()?,
                    Lifecycle::Deleted,
                    memory.status(),
                )?,
            );
        }
        AggregateChange::RestoreMemory {
            entity_id,
            expected_revision,
        } => {
            let memory = state
                .memories
                .get(&(workspace_id, entity_id))
                .cloned()
                .ok_or(ApplicationError::NotFound { entity: "memory" })?;
            require_revision(Some(memory.revision()), expected_revision, "memory")?;
            state.memories.insert(
                (workspace_id, entity_id),
                rehydrate_memory(
                    &memory,
                    expected_revision.next()?,
                    Lifecycle::Active,
                    memory.status(),
                )?,
            );
        }
        _ => return Err(ApplicationError::Internal),
    }
    Ok(())
}

fn rehydrate_memory(
    memory: &MemoryAssertion,
    revision: Revision,
    lifecycle: Lifecycle,
    status: MemoryStatus,
) -> Result<MemoryAssertion, ApplicationError> {
    Ok(MemoryAssertion::rehydrate(
        memory.id(),
        memory.workspace_id(),
        memory.statement().to_owned(),
        memory.normalized_subject().to_owned(),
        memory.normalized_predicate().to_owned(),
        memory.normalized_object().to_owned(),
        memory.sources().to_vec(),
        memory.supersedes(),
        status,
        revision,
        lifecycle,
    )?)
}

fn require_revision(
    actual: Option<Revision>,
    expected: Revision,
    entity: &'static str,
) -> Result<(), ApplicationError> {
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(ApplicationError::Conflict { entity })
    }
}

pub fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
