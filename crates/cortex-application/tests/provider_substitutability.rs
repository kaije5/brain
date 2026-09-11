//! Substitutability proof for the provider contracts (SCRUM-96).
//!
//! The approved SCRUM-90 design requires that "a complete in-memory fake
//! implements both ports" and that later Markdown adapters "pass the same
//! behavior contract without changing domain or application consumers". This
//! test is that proof: one generic consumer scenario runs unchanged against
//! two independently implemented in-memory providers with different
//! internals, asserting identical outcome shapes at every step. Any future
//! adapter (SCRUM-91's Markdown provider included) must satisfy the same
//! scenario without modifying the consumer.

#![allow(clippy::result_large_err)]

use std::num::NonZeroUsize;

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
use sha2::{Digest, Sha256};

const LIMIT: NonZeroUsize = NonZeroUsize::MIN;

fn scheduling() -> TaskSchedulingMetadata {
    TaskSchedulingMetadata::new(
        None,
        None,
        None,
        None,
        None,
        Option::<String>::None,
        Option::<String>::None,
    )
    .expect("empty scheduling is valid")
}

/// One full knowledge and task lifecycle expressed only in port terms.
#[allow(clippy::too_many_lines)] // The consumer contract is the review unit.
///
/// This function is the consumer contract: it never inspects provider
/// internals and must keep compiling and behaving identically for any
/// `KnowledgeProvider`/`TaskProvider` pair.
async fn consume_knowledge_and_tasks<P: KnowledgeProvider, T: TaskProvider>(
    knowledge: &P,
    tasks: &T,
    workspace_id: WorkspaceId,
) -> Result<(), ProviderError> {
    // Create → observed provenance with a first revision.
    let created = KnowledgeProvider::create(
        knowledge,
        KnowledgeCreate::new(workspace_id, OperationId::new(), "Title", "Body")?,
    )
    .await?;
    let resource = created.resource();
    let first_revision = created
        .current()
        .expect("create always observes current provenance")
        .observed_revision()
        .clone();

    // Read back the authoritative document.
    let read = KnowledgeProvider::get(knowledge, resource)
        .await?
        .expect("created document is found");
    assert_eq!(read.item().title(), "Title");
    assert_eq!(read.freshness(), ProviderFreshness::Current);

    // Bounded list search finds exactly the created document.
    let listed =
        KnowledgeProvider::search(knowledge, &KnowledgeQuery::list(workspace_id, LIMIT)?).await?;
    assert_eq!(listed.items().len(), 1);

    // Revision-aware update advances the observation.
    let updated = KnowledgeProvider::update(
        knowledge,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            first_revision.clone(),
            "Updated",
            "Body 2",
        )?,
    )
    .await?;
    let second_revision = updated
        .current()
        .expect("update always observes current provenance")
        .observed_revision()
        .clone();
    assert_ne!(first_revision, second_revision);

    // A stale expected revision is an explicit conflict, never a silent win.
    let stale = KnowledgeProvider::update(
        knowledge,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            first_revision,
            "Stale",
            "Stale body",
        )?,
    )
    .await;
    assert!(matches!(stale, Err(ProviderError::Conflict { .. })));

    // Text search reflects the update.
    let matched = KnowledgeProvider::search(
        knowledge,
        &KnowledgeQuery::new(workspace_id, "updated", LIMIT)?,
    )
    .await?;
    assert_eq!(matched.items().len(), 1);

    // Revision-aware delete removes the document.
    KnowledgeProvider::delete(
        knowledge,
        KnowledgeDelete::new(resource.clone(), OperationId::new(), second_revision)?,
    )
    .await?;
    assert!(KnowledgeProvider::get(knowledge, resource).await?.is_none());

    // Task lifecycle: create → complete (a revision-aware mutation) → delete.
    let task_created = TaskProvider::create(
        tasks,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            TaskId::new(),
            "Task",
            "Task body",
            ProviderTaskPriority::Normal,
            scheduling(),
        )?,
    )
    .await?;
    let task_resource = task_created.resource().clone();
    let task_revision = task_created
        .current()
        .expect("create always observes current provenance")
        .observed_revision()
        .clone();

    let completed = TaskProvider::complete(
        tasks,
        TaskComplete::new(
            task_resource.clone(),
            OperationId::new(),
            task_revision.clone(),
        )?,
    )
    .await?;
    let completed_revision = completed
        .current()
        .expect("complete always observes current provenance")
        .observed_revision()
        .clone();

    let task_read = TaskProvider::get(tasks, &task_resource)
        .await?
        .expect("completed task is found");
    assert_eq!(task_read.item().status(), ProviderTaskStatus::Completed);

    TaskProvider::delete(
        tasks,
        TaskDelete::new(task_resource, OperationId::new(), completed_revision)?,
    )
    .await?;
    let remaining = TaskProvider::search(
        tasks,
        &TaskQuery::new(workspace_id, Option::<String>::None, LIMIT)?,
    )
    .await?;
    assert_eq!(remaining.items().len(), 0);
    Ok(())
}

fn provenance(
    resource: ProviderResourceRef,
    revision: u64,
    title: &str,
    body: &str,
) -> Result<ProviderProvenance, ProviderError> {
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update([0]);
    hasher.update(body.as_bytes());
    Ok(ProviderProvenance::new(
        resource,
        ObservedRevision::new(format!("rev-{revision}")).map_err(|_| ProviderError::Internal)?,
        ContentHash::new(hasher.finalize().into()),
    ))
}

/// Fake A: `BTreeMap`-keyed storage.
mod map_backed {
    use super::{
        KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
        KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
        ProviderRead, ProviderTask, ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete,
        TaskProvider, TaskQuery, TaskUpdate,
    };
    use crate::{
        consume_scenario_support, provenance,
        provenance_support::{next_revision, require_kind},
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use cortex_domain::{ProviderResourceKind, ProviderResourceRef};

    #[derive(Default)]
    pub struct MapBackedProvider {
        knowledge: Mutex<BTreeMap<String, KnowledgeDocument>>,
        tasks: Mutex<BTreeMap<String, ProviderTask>>,
    }

    impl MapBackedProvider {
        fn knowledge_update(
            &self,
            input: &KnowledgeUpdate,
        ) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let current = store
                .get(input.resource().resource_id().as_str())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                input.title(),
                input.body(),
            )?;
            store.insert(
                input.resource().resource_id().as_str().to_owned(),
                KnowledgeDocument::new(new_provenance.clone(), input.title(), input.body())?,
            );
            ProviderMutation::updated(previous, new_provenance)
        }
    }

    impl KnowledgeProvider for MapBackedProvider {
        async fn get(
            &self,
            resource: &ProviderResourceRef,
        ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
            require_kind(resource, ProviderResourceKind::Knowledge)?;
            Ok(self
                .knowledge
                .lock()
                .expect("knowledge lock")
                .get(resource.resource_id().as_str())
                .cloned()
                .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
        }

        async fn search(
            &self,
            query: &KnowledgeQuery,
        ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
            let text = query.text().map(str::to_lowercase);
            let items = self
                .knowledge
                .lock()
                .expect("knowledge lock")
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
            ProviderPage::new(items, ProviderFreshness::Current)
        }

        async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let id = format!("k-{}", store.len() + 1);
            let resource = consume_scenario_support(
                input.workspace_id(),
                &id,
                ProviderResourceKind::Knowledge,
            )?;
            let new_provenance = provenance(resource, 1, input.title(), input.body())?;
            let document =
                KnowledgeDocument::new(new_provenance.clone(), input.title(), input.body())?;
            store.insert(id, document);
            Ok(ProviderMutation::created(new_provenance))
        }

        async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
            self.knowledge_update(&input)
        }

        async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let removed = store
                .remove(input.resource().resource_id().as_str())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            Ok(ProviderMutation::deleted(removed.provenance().clone()))
        }
    }

    impl TaskProvider for MapBackedProvider {
        async fn get(
            &self,
            resource: &ProviderResourceRef,
        ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
            require_kind(resource, ProviderResourceKind::Task)?;
            Ok(self
                .tasks
                .lock()
                .expect("task lock")
                .get(resource.resource_id().as_str())
                .cloned()
                .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
        }

        async fn search(
            &self,
            query: &TaskQuery,
        ) -> Result<ProviderPage<ProviderTask>, ProviderError> {
            let text = query.text().map(str::to_lowercase);
            let items = self
                .tasks
                .lock()
                .expect("task lock")
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
            ProviderPage::new(items, ProviderFreshness::Current)
        }

        async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let id = format!("t-{}", store.len() + 1);
            let resource =
                consume_scenario_support(input.workspace_id(), &id, ProviderResourceKind::Task)?;
            let new_provenance = provenance(resource, 1, input.title(), input.body())?;
            let task = ProviderTask::new(
                new_provenance.clone(),
                input.task_id(),
                input.title(),
                input.body(),
                ProviderTaskStatus::Todo,
                input.priority(),
                input.scheduling().clone(),
            )?;
            store.insert(id, task);
            Ok(ProviderMutation::created(new_provenance))
        }

        async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let current = store
                .get(input.resource().resource_id().as_str())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                input.title(),
                input.body(),
            )?;
            let updated = ProviderTask::new(
                new_provenance.clone(),
                current.task_id(),
                input.title(),
                input.body(),
                input.status(),
                input.priority(),
                input.scheduling().clone(),
            )?;
            store.insert(input.resource().resource_id().as_str().to_owned(), updated);
            ProviderMutation::updated(previous, new_provenance)
        }

        async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let current = store
                .get(input.resource().resource_id().as_str())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                current.title(),
                current.body(),
            )?;
            let completed = ProviderTask::new(
                new_provenance.clone(),
                current.task_id(),
                current.title(),
                current.body(),
                ProviderTaskStatus::Completed,
                current.priority(),
                current.scheduling().clone(),
            )?;
            store.insert(
                input.resource().resource_id().as_str().to_owned(),
                completed,
            );
            ProviderMutation::updated(previous, new_provenance)
        }

        async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let removed = store
                .remove(input.resource().resource_id().as_str())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            Ok(ProviderMutation::deleted(removed.provenance().clone()))
        }
    }
}

/// Fake B: `Vec`-backed linear storage, intentionally different internals.
mod vec_backed {
    use super::{
        KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
        KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
        ProviderRead, ProviderTask, ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete,
        TaskProvider, TaskQuery, TaskUpdate, consume_scenario_support, next_revision, provenance,
        require_kind,
    };
    use std::sync::Mutex;

    use cortex_domain::{ProviderResourceKind, ProviderResourceRef};

    #[derive(Default)]
    pub struct VecBackedProvider {
        knowledge: Mutex<Vec<KnowledgeDocument>>,
        tasks: Mutex<Vec<ProviderTask>>,
    }

    impl VecBackedProvider {
        fn knowledge_update(
            &self,
            input: &KnowledgeUpdate,
        ) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let index = store
                .iter()
                .position(|item| item.provenance().resource() == input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let current = &store[index];
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                input.title(),
                input.body(),
            )?;
            store[index] =
                KnowledgeDocument::new(new_provenance.clone(), input.title(), input.body())?;
            ProviderMutation::updated(previous, new_provenance)
        }
    }

    impl KnowledgeProvider for VecBackedProvider {
        async fn get(
            &self,
            resource: &ProviderResourceRef,
        ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
            require_kind(resource, ProviderResourceKind::Knowledge)?;
            Ok(self
                .knowledge
                .lock()
                .expect("knowledge lock")
                .iter()
                .find(|item| item.provenance().resource() == resource)
                .cloned()
                .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
        }

        async fn search(
            &self,
            query: &KnowledgeQuery,
        ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
            let text = query.text().map(str::to_lowercase);
            let items = self
                .knowledge
                .lock()
                .expect("knowledge lock")
                .iter()
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
            ProviderPage::new(items, ProviderFreshness::Current)
        }

        async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let resource = consume_scenario_support(
                input.workspace_id(),
                &format!("knowledge-{}", store.len() + 1),
                ProviderResourceKind::Knowledge,
            )?;
            let new_provenance = provenance(resource, 1, input.title(), input.body())?;
            let document =
                KnowledgeDocument::new(new_provenance.clone(), input.title(), input.body())?;
            store.push(document);
            Ok(ProviderMutation::created(new_provenance))
        }

        async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
            self.knowledge_update(&input)
        }

        async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.knowledge.lock().expect("knowledge lock");
            let index = store
                .iter()
                .position(|item| item.provenance().resource() == input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let removed = store.remove(index);
            Ok(ProviderMutation::deleted(removed.provenance().clone()))
        }
    }

    impl TaskProvider for VecBackedProvider {
        async fn get(
            &self,
            resource: &ProviderResourceRef,
        ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
            require_kind(resource, ProviderResourceKind::Task)?;
            Ok(self
                .tasks
                .lock()
                .expect("task lock")
                .iter()
                .find(|item| item.provenance().resource() == resource)
                .cloned()
                .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
        }

        async fn search(
            &self,
            query: &TaskQuery,
        ) -> Result<ProviderPage<ProviderTask>, ProviderError> {
            let text = query.text().map(str::to_lowercase);
            let items = self
                .tasks
                .lock()
                .expect("task lock")
                .iter()
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
            ProviderPage::new(items, ProviderFreshness::Current)
        }

        async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let resource = consume_scenario_support(
                input.workspace_id(),
                &format!("task-{}", store.len() + 1),
                ProviderResourceKind::Task,
            )?;
            let new_provenance = provenance(resource, 1, input.title(), input.body())?;
            let task = ProviderTask::new(
                new_provenance.clone(),
                input.task_id(),
                input.title(),
                input.body(),
                ProviderTaskStatus::Todo,
                input.priority(),
                input.scheduling().clone(),
            )?;
            store.push(task);
            Ok(ProviderMutation::created(new_provenance))
        }

        async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let index = store
                .iter()
                .position(|item| item.provenance().resource() == input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let current = store[index].clone();
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                input.title(),
                input.body(),
            )?;
            store[index] = ProviderTask::new(
                new_provenance.clone(),
                current.task_id(),
                input.title(),
                input.body(),
                input.status(),
                input.priority(),
                input.scheduling().clone(),
            )?;
            ProviderMutation::updated(previous, new_provenance)
        }

        async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let index = store
                .iter()
                .position(|item| item.provenance().resource() == input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let current = store[index].clone();
            let revision = next_revision(current.provenance(), input.expected_revision())?;
            let previous = current.provenance().clone();
            let new_provenance = provenance(
                input.resource().clone(),
                revision,
                current.title(),
                current.body(),
            )?;
            store[index] = ProviderTask::new(
                new_provenance.clone(),
                current.task_id(),
                current.title(),
                current.body(),
                ProviderTaskStatus::Completed,
                current.priority(),
                current.scheduling().clone(),
            )?;
            ProviderMutation::updated(previous, new_provenance)
        }

        async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
            let mut store = self.tasks.lock().expect("task lock");
            let index = store
                .iter()
                .position(|item| item.provenance().resource() == input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
            let removed = store.remove(index);
            Ok(ProviderMutation::deleted(removed.provenance().clone()))
        }
    }
}

/// Shared helpers for the two independent fakes.
mod provenance_support {
    use cortex_application::ProviderError;
    use cortex_domain::{ObservedRevision, ProviderProvenance, ProviderResourceKind};

    pub fn require_kind(
        resource: &cortex_domain::ProviderResourceRef,
        expected: ProviderResourceKind,
    ) -> Result<(), ProviderError> {
        if resource.kind() != expected {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        Ok(())
    }

    pub fn next_revision(
        current: &ProviderProvenance,
        expected: &ObservedRevision,
    ) -> Result<u64, ProviderError> {
        if current.observed_revision() != expected {
            return Err(ProviderError::Conflict {
                current: current.clone(),
            });
        }
        let parsed = current
            .observed_revision()
            .as_str()
            .trim_start_matches("rev-")
            .parse::<u64>()
            .unwrap_or(0);
        Ok(parsed + 1)
    }
}

use provenance_support::{next_revision, require_kind};

fn consume_scenario_support(
    workspace_id: WorkspaceId,
    resource_id: &str,
    kind: ProviderResourceKind,
) -> Result<ProviderResourceRef, ProviderError> {
    Ok(ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("substitutability").map_err(|_| ProviderError::Internal)?,
        ProviderResourceId::new(resource_id).map_err(|_| ProviderError::Internal)?,
        kind,
    ))
}

#[tokio::test]
async fn map_backed_fake_satisfies_the_full_consumer_contract() {
    let provider = map_backed::MapBackedProvider::default();
    consume_knowledge_and_tasks(&provider, &provider, WorkspaceId::new())
        .await
        .expect("map-backed fake satisfies the consumer contract");
}

#[tokio::test]
async fn vec_backed_fake_satisfies_the_full_consumer_contract() {
    let provider = vec_backed::VecBackedProvider::default();
    consume_knowledge_and_tasks(&provider, &provider, WorkspaceId::new())
        .await
        .expect("vec-backed fake satisfies the consumer contract");
}
