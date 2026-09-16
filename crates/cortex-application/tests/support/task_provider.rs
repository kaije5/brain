//! In-memory [`TaskProvider`] fake shared by SCRUM-87 integration tests.
#![allow(clippy::result_large_err, dead_code)]

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use cortex_application::{
    ProviderError, ProviderMutation, ProviderPage, ProviderRead, ProviderTask, TaskComplete,
    TaskCreate, TaskDelete, TaskProvider, TaskQuery, TaskUpdate,
};
use cortex_domain::{
    ContentHash, ObservedRevision, OperationId, ProviderProvenance, ProviderResourceKind,
    ProviderResourceRef, WorkspaceId,
};

#[derive(Clone, Default)]
pub struct FakeTaskProvider {
    tasks: Arc<Mutex<BTreeMap<String, ProviderTask>>>,
    fail_reads: Arc<Mutex<bool>>,
}

pub fn provenance(
    resource_id: &str,
    revision: u64,
    workspace_id: WorkspaceId,
) -> ProviderProvenance {
    ProviderProvenance::new(
        ProviderResourceRef::new(
            workspace_id,
            cortex_domain::ProviderId::new("markdown-vault").expect("provider id"),
            cortex_domain::ProviderResourceId::new(resource_id).expect("resource id"),
            ProviderResourceKind::Task,
        ),
        ObservedRevision::new(format!("rev-{revision}")).expect("revision"),
        ContentHash::new([u8::try_from(revision).unwrap_or(u8::MAX); 32]),
    )
}

pub fn make_task(
    resource_id: &str,
    task_id: cortex_domain::TaskId,
    revision: u64,
    workspace_id: WorkspaceId,
    priority: cortex_application::ProviderTaskPriority,
    scheduling: cortex_application::TaskSchedulingMetadata,
) -> ProviderTask {
    ProviderTask::new(
        provenance(resource_id, revision, workspace_id),
        task_id,
        format!("task {resource_id}"),
        "body",
        cortex_application::ProviderTaskStatus::Todo,
        priority,
        scheduling,
    )
    .expect("task")
}

impl FakeTaskProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, task: ProviderTask) {
        let resource = task
            .provenance()
            .resource()
            .resource_id()
            .as_str()
            .to_owned();
        self.tasks
            .lock()
            .expect("tasks mutex")
            .insert(resource, task);
    }

    /// Simulates a vault rename/move: same task identity, new resource path.
    pub fn rename(
        &self,
        from: &str,
        to: &str,
        workspace_id: WorkspaceId,
    ) -> Result<(), ProviderError> {
        let mut tasks = self.tasks.lock().expect("tasks mutex");
        let mut task = tasks.remove(from).ok_or_else(|| ProviderError::NotFound {
            resource: ProviderResourceRef::new(
                workspace_id,
                cortex_domain::ProviderId::new("markdown-vault").expect("provider id"),
                cortex_domain::ProviderResourceId::new(from).expect("resource id"),
                ProviderResourceKind::Task,
            ),
        })?;
        let provenance = ProviderProvenance::new(
            ProviderResourceRef::new(
                workspace_id,
                cortex_domain::ProviderId::new("markdown-vault").expect("provider id"),
                cortex_domain::ProviderResourceId::new(to).expect("resource id"),
                ProviderResourceKind::Task,
            ),
            ObservedRevision::new("renamed").expect("revision"),
            ContentHash::new([7; 32]),
        );
        task = ProviderTask::new(
            provenance,
            task.task_id(),
            task.title(),
            task.body(),
            task.status(),
            task.priority(),
            cortex_application::TaskSchedulingMetadata::new(
                task.scheduling().due_at(),
                task.scheduling().deadline_at(),
                task.scheduling().duration_minutes(),
                task.scheduling().earliest_start(),
                task.scheduling().split(),
                task.scheduling().project(),
                task.scheduling().context(),
            )
            .expect("scheduling"),
        )
        .expect("renamed task");
        tasks.insert(to.to_owned(), task);
        Ok(())
    }

    pub fn set_fail_reads(&self, fail: bool) {
        *self.fail_reads.lock().expect("fail mutex") = fail;
    }

    #[must_use]
    pub fn task_count(&self) -> usize {
        self.tasks.lock().expect("tasks mutex").len()
    }
}

impl TaskProvider for FakeTaskProvider {
    async fn get(
        &self,
        _resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        unimplemented!("not exercised by planning/review flows under test")
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        if *self.fail_reads.lock().expect("fail mutex") {
            return Err(ProviderError::Unavailable);
        }
        let items: Vec<ProviderTask> = self
            .tasks
            .lock()
            .expect("tasks mutex")
            .values()
            .filter(|task| task.provenance().resource().workspace_id() == query.workspace_id())
            .cloned()
            .collect();
        ProviderPage::new(items, cortex_application::ProviderFreshness::Current)
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        let task = ProviderTask::new(
            provenance(
                &format!("tasks/{}.md", uuid::Uuid::from(input.task_id())),
                1,
                input.workspace_id(),
            ),
            input.task_id(),
            input.title(),
            input.body(),
            cortex_application::ProviderTaskStatus::Todo,
            input.priority(),
            cortex_application::TaskSchedulingMetadata::new(
                input.scheduling().due_at(),
                input.scheduling().deadline_at(),
                input.scheduling().duration_minutes(),
                input.scheduling().earliest_start(),
                input.scheduling().split(),
                input.scheduling().project(),
                input.scheduling().context(),
            )
            .expect("scheduling"),
        )
        .expect("created task");
        self.insert(task);
        let current = self
            .tasks
            .lock()
            .expect("tasks mutex")
            .values()
            .find(|task| task.task_id() == input.task_id())
            .expect("inserted")
            .provenance()
            .clone();
        Ok(ProviderMutation::created(current))
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        let _ = (input, OperationId::new(), NonZeroUsize::MIN);
        Err(ProviderError::Internal)
    }

    async fn complete(&self, _input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        Err(ProviderError::Internal)
    }

    async fn delete(&self, _input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        Err(ProviderError::Internal)
    }
}
