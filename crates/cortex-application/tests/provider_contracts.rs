#![allow(clippy::result_large_err)]

use std::{collections::BTreeMap, num::NonZeroUsize, sync::Mutex};

use cortex_application::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
    KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead, ProviderTask, ProviderTaskPriority, ProviderTaskStatus, TaskComplete, TaskCreate,
    TaskDelete, TaskProvider, TaskQuery, TaskSchedulingMetadata, TaskUpdate,
};
use cortex_domain::{
    ContentHash, ObservedRevision, OperationId, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
};

#[derive(Default)]
struct State {
    sequence: u64,
    knowledge: BTreeMap<ProviderResourceRef, KnowledgeDocument>,
    tasks: BTreeMap<ProviderResourceRef, ProviderTask>,
}

struct FakeProvider {
    state: Mutex<State>,
    freshness: ProviderFreshness,
    unavailable: bool,
}

impl FakeProvider {
    fn new(freshness: ProviderFreshness) -> Self {
        Self {
            state: Mutex::new(State::default()),
            freshness,
            unavailable: false,
        }
    }

    fn unavailable() -> Self {
        Self {
            state: Mutex::new(State::default()),
            freshness: ProviderFreshness::Current,
            unavailable: true,
        }
    }

    fn check(&self) -> Result<(), ProviderError> {
        if self.unavailable {
            Err(ProviderError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn resource(
        workspace: WorkspaceId,
        sequence: u64,
        kind: ProviderResourceKind,
    ) -> ProviderResourceRef {
        ProviderResourceRef::new(
            workspace,
            ProviderId::new("fake").unwrap(),
            ProviderResourceId::new(format!("resource-{sequence}")).unwrap(),
            kind,
        )
    }

    fn provenance(resource: ProviderResourceRef, revision: u64) -> ProviderProvenance {
        let mut hash = [0_u8; 32];
        hash[..8].copy_from_slice(&revision.to_le_bytes());
        ProviderProvenance::new(
            resource,
            ObservedRevision::new(format!("rev-{revision}")).unwrap(),
            ContentHash::new(hash),
        )
    }

    fn revision(
        current: &ProviderProvenance,
        expected: &ObservedRevision,
    ) -> Result<u64, ProviderError> {
        if current.observed_revision() != expected {
            return Err(ProviderError::Conflict {
                current: current.clone(),
            });
        }
        Ok(current
            .observed_revision()
            .as_str()
            .trim_start_matches("rev-")
            .parse::<u64>()
            .unwrap()
            + 1)
    }

    async fn create<I: CreateInput>(&self, input: I) -> Result<ProviderMutation, ProviderError> {
        input.create_with(self).await
    }

    async fn update<I: UpdateInput>(&self, input: I) -> Result<ProviderMutation, ProviderError> {
        input.update_with(self).await
    }

    async fn delete<I: DeleteInput>(&self, input: I) -> Result<ProviderMutation, ProviderError> {
        input.delete_with(self).await
    }

    async fn search<Q: SearchInput>(
        &self,
        query: &Q,
    ) -> Result<ProviderPage<Q::Item>, ProviderError> {
        query.search_with(self).await
    }
}

#[allow(async_fn_in_trait)]
trait CreateInput {
    async fn create_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError>;
}

impl CreateInput for KnowledgeCreate {
    async fn create_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        KnowledgeProvider::create(provider, self).await
    }
}

impl CreateInput for TaskCreate {
    async fn create_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        TaskProvider::create(provider, self).await
    }
}

#[allow(async_fn_in_trait)]
trait UpdateInput {
    async fn update_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError>;
}

impl UpdateInput for KnowledgeUpdate {
    async fn update_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        KnowledgeProvider::update(provider, self).await
    }
}

impl UpdateInput for TaskUpdate {
    async fn update_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        TaskProvider::update(provider, self).await
    }
}

#[allow(async_fn_in_trait)]
trait DeleteInput {
    async fn delete_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError>;
}

impl DeleteInput for KnowledgeDelete {
    async fn delete_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        KnowledgeProvider::delete(provider, self).await
    }
}

impl DeleteInput for TaskDelete {
    async fn delete_with(self, provider: &FakeProvider) -> Result<ProviderMutation, ProviderError> {
        TaskProvider::delete(provider, self).await
    }
}

#[allow(async_fn_in_trait)]
trait SearchInput {
    type Item;
    async fn search_with(
        &self,
        provider: &FakeProvider,
    ) -> Result<ProviderPage<Self::Item>, ProviderError>;
}

impl SearchInput for KnowledgeQuery {
    type Item = KnowledgeDocument;

    async fn search_with(
        &self,
        provider: &FakeProvider,
    ) -> Result<ProviderPage<Self::Item>, ProviderError> {
        KnowledgeProvider::search(provider, self).await
    }
}

impl SearchInput for TaskQuery {
    type Item = ProviderTask;

    async fn search_with(
        &self,
        provider: &FakeProvider,
    ) -> Result<ProviderPage<Self::Item>, ProviderError> {
        TaskProvider::search(provider, self).await
    }
}

async fn read_knowledge<P: KnowledgeProvider>(
    provider: &P,
    resource: &ProviderResourceRef,
) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
    provider.get(resource).await
}

async fn read_task<P: TaskProvider>(
    provider: &P,
    resource: &ProviderResourceRef,
) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
    provider.get(resource).await
}

impl KnowledgeProvider for FakeProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
        self.check()?;
        if resource.kind() != ProviderResourceKind::Knowledge {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        Ok(self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .knowledge
            .get(resource)
            .cloned()
            .map(|item| ProviderRead::new(item, self.freshness)))
    }

    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
        self.check()?;
        let text = query.text().to_lowercase();
        let items = self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .knowledge
            .values()
            .filter(|item| item.provenance().resource().workspace_id() == query.workspace_id())
            .filter(|item| {
                item.title().to_lowercase().contains(&text)
                    || item.body().to_lowercase().contains(&text)
            })
            .take(query.limit().get())
            .cloned()
            .collect();
        ProviderPage::new(items, self.freshness)
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        state.sequence += 1;
        let resource = Self::resource(
            input.workspace_id(),
            state.sequence,
            ProviderResourceKind::Knowledge,
        );
        let provenance = Self::provenance(resource.clone(), 1);
        let document = KnowledgeDocument::new(provenance.clone(), input.title(), input.body())?;
        state.knowledge.insert(resource, document);
        Ok(ProviderMutation::created(provenance))
    }

    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current = state
            .knowledge
            .get(input.resource())
            .cloned()
            .ok_or_else(|| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(input.resource().clone(), revision);
        state.knowledge.insert(
            input.resource().clone(),
            KnowledgeDocument::new(provenance.clone(), input.title(), input.body())?,
        );
        ProviderMutation::updated(previous, provenance)
    }

    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .knowledge
                .get(input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        Self::revision(current.provenance(), input.expected_revision())?;
        let removed = state
            .knowledge
            .remove(input.resource())
            .ok_or(ProviderError::Internal)?;
        Ok(ProviderMutation::deleted(removed.provenance().clone()))
    }
}

impl TaskProvider for FakeProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        self.check()?;
        if resource.kind() != ProviderResourceKind::Task {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        Ok(self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .tasks
            .get(resource)
            .cloned()
            .map(|item| ProviderRead::new(item, self.freshness)))
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        self.check()?;
        let text = query.text().map(str::to_lowercase);
        let items = self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .tasks
            .values()
            .filter(|item| item.provenance().resource().workspace_id() == query.workspace_id())
            .filter(|item| {
                text.as_ref().is_none_or(|text| {
                    item.title().to_lowercase().contains(text)
                        || item.body().to_lowercase().contains(text)
                })
            })
            .take(query.limit().get())
            .cloned()
            .collect();
        ProviderPage::new(items, self.freshness)
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        state.sequence += 1;
        let resource = Self::resource(
            input.workspace_id(),
            state.sequence,
            ProviderResourceKind::Task,
        );
        let provenance = Self::provenance(resource.clone(), 1);
        let task = ProviderTask::new(
            provenance.clone(),
            input.task_id(),
            input.title(),
            input.body(),
            ProviderTaskStatus::Todo,
            input.priority(),
            input.scheduling().clone(),
        )?;
        state.tasks.insert(resource, task);
        Ok(ProviderMutation::created(provenance))
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .tasks
                .get(input.resource())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(input.resource().clone(), revision);
        let updated = ProviderTask::new(
            provenance.clone(),
            current.task_id(),
            input.title(),
            input.body(),
            input.status(),
            input.priority(),
            input.scheduling().clone(),
        )?;
        state.tasks.insert(input.resource().clone(), updated);
        ProviderMutation::updated(previous, provenance)
    }

    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .tasks
                .get(input.resource())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(input.resource().clone(), revision);
        let completed = ProviderTask::new(
            provenance.clone(),
            current.task_id(),
            current.title(),
            current.body(),
            ProviderTaskStatus::Completed,
            current.priority(),
            current.scheduling().clone(),
        )?;
        state.tasks.insert(input.resource().clone(), completed);
        ProviderMutation::updated(previous, provenance)
    }

    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        self.check()?;
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current = state
            .tasks
            .get(input.resource())
            .ok_or_else(|| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        Self::revision(current.provenance(), input.expected_revision())?;
        let removed = state
            .tasks
            .remove(input.resource())
            .ok_or(ProviderError::Internal)?;
        Ok(ProviderMutation::deleted(removed.provenance().clone()))
    }
}

fn current(mutation: &ProviderMutation) -> ProviderProvenance {
    mutation.current().unwrap().clone()
}

#[tokio::test]
async fn knowledge_create_get_and_search_return_the_same_document() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let workspace = WorkspaceId::new();
    let created = provider
        .create(
            KnowledgeCreate::new(workspace, OperationId::new(), "Runbook", "recovery steps")
                .unwrap(),
        )
        .await
        .unwrap();
    let provenance = current(&created);
    let read = read_knowledge(&provider, provenance.resource())
        .await
        .unwrap()
        .unwrap();
    let page = provider
        .search(
            &KnowledgeQuery::new(workspace, "recovery", NonZeroUsize::new(10).unwrap()).unwrap(),
        )
        .await
        .unwrap();
    assert!(created.previous().is_none());
    assert_eq!(page.items(), std::slice::from_ref(read.item()));
}

#[tokio::test]
async fn knowledge_update_returns_previous_and_current_evidence() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let created = provider
        .create(KnowledgeCreate::new(WorkspaceId::new(), OperationId::new(), "A", "one").unwrap())
        .await
        .unwrap();
    let previous = current(&created);
    let updated = provider
        .update(
            KnowledgeUpdate::new(
                previous.resource().clone(),
                OperationId::new(),
                previous.observed_revision().clone(),
                "A2",
                "two",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.previous(), Some(&previous));
    assert_ne!(
        updated.current().unwrap().observed_revision(),
        previous.observed_revision()
    );
    assert_ne!(
        updated.current().unwrap().content_hash(),
        previous.content_hash()
    );
}

#[tokio::test]
async fn stale_knowledge_update_and_delete_preserve_state_before_valid_delete() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let first = current(
        &provider
            .create(
                KnowledgeCreate::new(WorkspaceId::new(), OperationId::new(), "A", "one").unwrap(),
            )
            .await
            .unwrap(),
    );
    let latest = current(
        &provider
            .update(
                KnowledgeUpdate::new(
                    first.resource().clone(),
                    OperationId::new(),
                    first.observed_revision().clone(),
                    "A",
                    "two",
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let stale_update = KnowledgeUpdate::new(
        first.resource().clone(),
        OperationId::new(),
        first.observed_revision().clone(),
        "bad",
        "bad",
    )
    .unwrap();
    let stale_delete = KnowledgeDelete::new(
        first.resource().clone(),
        OperationId::new(),
        first.observed_revision().clone(),
    )
    .unwrap();
    assert_eq!(
        provider.update(stale_update).await,
        Err(ProviderError::Conflict {
            current: latest.clone()
        })
    );
    assert_eq!(
        provider.delete(stale_delete).await,
        Err(ProviderError::Conflict {
            current: latest.clone()
        })
    );
    assert_eq!(
        read_knowledge(&provider, latest.resource())
            .await
            .unwrap()
            .unwrap()
            .item()
            .body(),
        "two"
    );
    let deleted = provider
        .delete(
            KnowledgeDelete::new(
                latest.resource().clone(),
                OperationId::new(),
                latest.observed_revision().clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.previous(), Some(&latest));
    assert!(
        read_knowledge(&provider, latest.resource())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn task_crud_completion_preserves_stable_task_identity() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let workspace = WorkspaceId::new();
    let task_id = TaskId::new();
    let first = current(
        &provider
            .create(
                TaskCreate::new(
                    workspace,
                    OperationId::new(),
                    task_id,
                    "Ship",
                    "",
                    ProviderTaskPriority::High,
                    TaskSchedulingMetadata::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let listed = provider
        .search(&TaskQuery::new(workspace, None::<String>, NonZeroUsize::new(5).unwrap()).unwrap())
        .await
        .unwrap();
    assert_eq!(listed.items()[0].task_id(), task_id);
    let second = current(
        &provider
            .update(
                TaskUpdate::new(
                    first.resource().clone(),
                    OperationId::new(),
                    first.observed_revision().clone(),
                    "Ship now",
                    "details",
                    ProviderTaskStatus::InProgress,
                    ProviderTaskPriority::Urgent,
                    TaskSchedulingMetadata::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let third = current(
        &provider
            .complete(
                TaskComplete::new(
                    second.resource().clone(),
                    OperationId::new(),
                    second.observed_revision().clone(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let read = read_task(&provider, third.resource())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.item().task_id(), task_id);
    assert_eq!(read.item().status(), ProviderTaskStatus::Completed);
    let deleted = provider
        .delete(
            TaskDelete::new(
                third.resource().clone(),
                OperationId::new(),
                third.observed_revision().clone(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.previous(), Some(&third));
    assert!(
        read_task(&provider, third.resource())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn stale_task_update_complete_and_delete_preserve_state() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let first = current(
        &provider
            .create(
                TaskCreate::new(
                    WorkspaceId::new(),
                    OperationId::new(),
                    TaskId::new(),
                    "A",
                    "",
                    ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let latest = current(
        &provider
            .update(
                TaskUpdate::new(
                    first.resource().clone(),
                    OperationId::new(),
                    first.observed_revision().clone(),
                    "B",
                    "",
                    ProviderTaskStatus::InProgress,
                    ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap(),
    );
    let update = TaskUpdate::new(
        first.resource().clone(),
        OperationId::new(),
        first.observed_revision().clone(),
        "bad",
        "",
        ProviderTaskStatus::Cancelled,
        ProviderTaskPriority::Low,
        TaskSchedulingMetadata::default(),
    )
    .unwrap();
    let complete = TaskComplete::new(
        first.resource().clone(),
        OperationId::new(),
        first.observed_revision().clone(),
    )
    .unwrap();
    let delete = TaskDelete::new(
        first.resource().clone(),
        OperationId::new(),
        first.observed_revision().clone(),
    )
    .unwrap();
    assert!(matches!(
        provider.update(update).await,
        Err(ProviderError::Conflict { .. })
    ));
    assert!(matches!(
        provider.complete(complete).await,
        Err(ProviderError::Conflict { .. })
    ));
    assert!(matches!(
        provider.delete(delete).await,
        Err(ProviderError::Conflict { .. })
    ));
    assert_eq!(
        read_task(&provider, latest.resource())
            .await
            .unwrap()
            .unwrap()
            .item()
            .title(),
        "B"
    );
}

#[tokio::test]
async fn unavailable_provider_returns_typed_errors_for_reads_and_mutations() {
    let provider = FakeProvider::unavailable();
    let resource = FakeProvider::resource(WorkspaceId::new(), 1, ProviderResourceKind::Knowledge);
    assert_eq!(
        read_knowledge(&provider, &resource).await,
        Err(ProviderError::Unavailable)
    );
    assert_eq!(
        provider
            .create(
                KnowledgeCreate::new(resource.workspace_id(), OperationId::new(), "A", "").unwrap()
            )
            .await,
        Err(ProviderError::Unavailable)
    );
}

#[tokio::test]
async fn wrong_resource_kinds_are_rejected_before_reads() {
    let provider = FakeProvider::new(ProviderFreshness::Current);
    let workspace = WorkspaceId::new();
    let knowledge = FakeProvider::resource(workspace, 1, ProviderResourceKind::Knowledge);
    let task = FakeProvider::resource(workspace, 2, ProviderResourceKind::Task);
    assert!(matches!(
        read_knowledge(&provider, &task).await,
        Err(ProviderError::Validation {
            field: "resource_kind"
        })
    ));
    assert!(matches!(
        read_task(&provider, &knowledge).await,
        Err(ProviderError::Validation {
            field: "resource_kind"
        })
    ));
}

#[tokio::test]
async fn searches_respect_requested_limits_and_explicit_freshness() {
    let provider = FakeProvider::new(ProviderFreshness::Stale);
    let workspace = WorkspaceId::new();
    for title in ["one", "two", "three"] {
        provider
            .create(KnowledgeCreate::new(workspace, OperationId::new(), title, "match").unwrap())
            .await
            .unwrap();
    }
    let page = provider
        .search(&KnowledgeQuery::new(workspace, "match", NonZeroUsize::new(2).unwrap()).unwrap())
        .await
        .unwrap();
    assert_eq!(page.items().len(), 2);
    assert_eq!(page.freshness(), ProviderFreshness::Stale);
}
