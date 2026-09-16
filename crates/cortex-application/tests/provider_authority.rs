//! Provider-backed application authority (SCRUM-116).
//!
//! Every user-content command evaluates policy, preserves operation identity
//! with idempotent replay, forwards the caller's expected revision untouched,
//! and returns typed results — without touching `SQLite` note/task rows.

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use cortex_application::{
    ApplicationError, AuditPort, Capability, CapabilityGrant, CommandContext, GrantPolicy,
    KnowledgeCreate, KnowledgeDelete, KnowledgeProvider, KnowledgeQuery, KnowledgeUpdate,
    ProviderAuthority, ProviderError, ProviderFreshness, ProviderMutation, ProviderOperationLog,
    ProviderOperationRecord, ProviderPage, ProviderRead, ProviderTask, ProviderTaskPriority,
    ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete, TaskProvider, TaskQuery,
    TaskSchedulingMetadata, TaskUpdate,
};
use cortex_domain::{
    AuditEvent, AuditResult, ObservedRevision, OperationId, PrincipalId, ProviderId,
    ProviderProvenance, ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
};
use uuid::Uuid;

const PROVIDER: &str = "fake-vault";

fn context() -> CommandContext {
    CommandContext::from_authenticated(
        WorkspaceId::new(),
        PrincipalId::new(),
        OperationId::new(),
        Uuid::now_v7(),
    )
}

/// The same authenticated principal and workspace with a fresh operation
/// identity, for sequences of distinct operations.
fn next_operation(context: &CommandContext) -> CommandContext {
    CommandContext::from_authenticated(
        context.workspace_id,
        context.principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn granted(
    context: &CommandContext,
    capabilities: impl IntoIterator<Item = Capability>,
) -> GrantPolicy {
    GrantPolicy::new(capabilities.into_iter().map(|capability| {
        CapabilityGrant::new(context.workspace_id, context.principal_id, capability)
    }))
}

fn revision(value: &str) -> ObservedRevision {
    ObservedRevision::new(value.to_owned()).expect("valid revision")
}

/// In-memory knowledge provider with optimistic revision gating.
#[derive(Default)]
struct FakeKnowledge {
    documents: Mutex<BTreeMap<String, (String, String, String)>>,
    dispatches: AtomicUsize,
}

impl KnowledgeProvider for FakeKnowledge {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<cortex_application::KnowledgeDocument>>, ProviderError> {
        let documents = self.documents.lock().expect("locked");
        Ok(documents
            .get(resource.resource_id().as_str())
            .map(|(title, body, revision)| {
                let provenance = provenance(resource, revision);
                let document = cortex_application::KnowledgeDocument::new(
                    provenance,
                    title.clone(),
                    body.clone(),
                )
                .expect("valid document");
                ProviderRead::new(document, ProviderFreshness::Current)
            }))
    }

    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<cortex_application::KnowledgeDocument>, ProviderError> {
        let documents = self.documents.lock().expect("locked");
        let items = documents
            .iter()
            .filter(|(_, (title, body, _))| {
                query
                    .text()
                    .is_none_or(|text| title.contains(text) || body.contains(text))
            })
            .map(|(id, (title, body, revision))| {
                let resource =
                    resource_from(query.workspace_id(), ProviderResourceKind::Knowledge, id);

                cortex_application::KnowledgeDocument::new(
                    provenance(&resource, revision),
                    title.clone(),
                    body.clone(),
                )
                .expect("valid document")
            })
            .collect();
        ProviderPage::new(items, ProviderFreshness::Current)
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = format!("doc:{}", input.title());
        let mut documents = self.documents.lock().expect("locked");
        if documents.contains_key(&id) {
            return Err(ProviderError::Conflict {
                current: provenance(
                    &resource_from(input.workspace_id(), ProviderResourceKind::Knowledge, &id),
                    "rev-0",
                ),
            });
        }
        documents.insert(
            id.clone(),
            (
                input.title().to_owned(),
                input.body().to_owned(),
                "rev-1".to_owned(),
            ),
        );
        Ok(ProviderMutation::created(provenance(
            &resource_from(input.workspace_id(), ProviderResourceKind::Knowledge, &id),
            "rev-1",
        )))
    }

    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = input.resource().resource_id().as_str().to_owned();
        let mut documents = self.documents.lock().expect("locked");
        let Some((title, body, revision)) = documents.get_mut(&id) else {
            return Err(ProviderError::NotFound {
                resource: input.resource().clone(),
            });
        };
        if *revision != input.expected_revision().as_str() {
            return Err(ProviderError::Conflict {
                current: provenance(input.resource(), revision),
            });
        }
        input.title().clone_into(title);
        input.body().clone_into(body);
        "rev-2".clone_into(revision);
        ProviderMutation::updated(
            provenance(input.resource(), input.expected_revision().as_str()),
            provenance(input.resource(), "rev-2"),
        )
    }

    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = input.resource().resource_id().as_str().to_owned();
        let mut documents = self.documents.lock().expect("locked");
        let Some((_, _, revision)) = documents.remove(&id) else {
            return Err(ProviderError::NotFound {
                resource: input.resource().clone(),
            });
        };
        if revision != input.expected_revision().as_str() {
            return Err(ProviderError::Conflict {
                current: provenance(input.resource(), &revision),
            });
        }
        Ok(ProviderMutation::deleted(provenance(
            input.resource(),
            &revision,
        )))
    }
}

/// In-memory task provider with optimistic revision gating.
#[derive(Default)]
struct FakeTasks {
    tasks: Mutex<BTreeMap<String, (ProviderTaskStatus, String, String)>>,
    dispatches: AtomicUsize,
}

impl TaskProvider for FakeTasks {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        let tasks = self.tasks.lock().expect("locked");
        Ok(tasks
            .get(resource.resource_id().as_str())
            .map(|(status, title, revision)| {
                let task = ProviderTask::new(
                    provenance(resource, revision),
                    task_id_of(resource),
                    title.clone(),
                    String::new(),
                    *status,
                    ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::default(),
                )
                .expect("valid task");
                ProviderRead::new(task, ProviderFreshness::Current)
            }))
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        let tasks = self.tasks.lock().expect("locked");
        let items = tasks
            .iter()
            .filter(|(_, (status, title, _))| {
                query.text().is_none_or(|text| {
                    title.contains(text) || format!("{status:?}").to_lowercase().contains(text)
                })
            })
            .map(|(id, (status, title, revision))| {
                let resource = resource_from(query.workspace_id(), ProviderResourceKind::Task, id);
                ProviderTask::new(
                    provenance(&resource, revision),
                    task_id_of(&resource),
                    title.clone(),
                    String::new(),
                    *status,
                    ProviderTaskPriority::Normal,
                    TaskSchedulingMetadata::default(),
                )
                .expect("valid task")
            })
            .collect();
        ProviderPage::new(items, ProviderFreshness::Current)
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = format!("task:{}", Uuid::from(input.task_id()));
        let mut tasks = self.tasks.lock().expect("locked");
        if tasks.contains_key(&id) {
            return Err(ProviderError::Conflict {
                current: provenance(
                    &resource_from(input.workspace_id(), ProviderResourceKind::Task, &id),
                    "rev-0",
                ),
            });
        }
        tasks.insert(
            id.clone(),
            (
                ProviderTaskStatus::Todo,
                input.title().to_owned(),
                "rev-1".to_owned(),
            ),
        );
        Ok(ProviderMutation::created(provenance(
            &resource_from(input.workspace_id(), ProviderResourceKind::Task, &id),
            "rev-1",
        )))
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = input.resource().resource_id().as_str().to_owned();
        let mut tasks = self.tasks.lock().expect("locked");
        let Some((_, title, revision)) = tasks.get_mut(&id) else {
            return Err(ProviderError::NotFound {
                resource: input.resource().clone(),
            });
        };
        if *revision != input.expected_revision().as_str() {
            return Err(ProviderError::Conflict {
                current: provenance(input.resource(), revision),
            });
        }
        input.title().clone_into(title);
        "rev-2".clone_into(revision);
        ProviderMutation::updated(
            provenance(input.resource(), input.expected_revision().as_str()),
            provenance(input.resource(), "rev-2"),
        )
    }

    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = input.resource().resource_id().as_str().to_owned();
        let mut tasks = self.tasks.lock().expect("locked");
        let Some((status, _, revision)) = tasks.get_mut(&id) else {
            return Err(ProviderError::NotFound {
                resource: input.resource().clone(),
            });
        };
        if *revision != input.expected_revision().as_str() {
            return Err(ProviderError::Conflict {
                current: provenance(input.resource(), revision),
            });
        }
        *status = ProviderTaskStatus::Completed;
        "rev-2".clone_into(revision);
        ProviderMutation::updated(
            provenance(input.resource(), input.expected_revision().as_str()),
            provenance(input.resource(), "rev-2"),
        )
    }

    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let id = input.resource().resource_id().as_str().to_owned();
        let mut tasks = self.tasks.lock().expect("locked");
        let Some((_, _, revision)) = tasks.remove(&id) else {
            return Err(ProviderError::NotFound {
                resource: input.resource().clone(),
            });
        };
        if revision != input.expected_revision().as_str() {
            return Err(ProviderError::Conflict {
                current: provenance(input.resource(), &revision),
            });
        }
        Ok(ProviderMutation::deleted(provenance(
            input.resource(),
            &revision,
        )))
    }
}

#[derive(Default)]
struct FakeOperationLog {
    records: Mutex<BTreeMap<Uuid, ProviderOperationRecord>>,
}

impl ProviderOperationLog for FakeOperationLog {
    async fn find(
        &self,
        _workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<ProviderOperationRecord>, ApplicationError> {
        Ok(self
            .records
            .lock()
            .expect("locked")
            .get(&Uuid::from(operation_id))
            .cloned())
    }

    async fn record(
        &self,
        _workspace_id: WorkspaceId,
        operation_id: OperationId,
        record: ProviderOperationRecord,
    ) -> Result<(), ApplicationError> {
        let mut records = self.records.lock().expect("locked");
        if records.insert(Uuid::from(operation_id), record).is_some() {
            return Err(ApplicationError::Internal);
        }
        Ok(())
    }
}

#[derive(Default)]
struct RecordingAudit {
    succeeded: AtomicUsize,
    rejected: AtomicUsize,
    failed: AtomicUsize,
}

impl AuditPort for RecordingAudit {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError> {
        match event.result {
            AuditResult::Succeeded => {
                self.succeeded.fetch_add(1, Ordering::SeqCst);
            }
            AuditResult::Rejected => {
                self.rejected.fetch_add(1, Ordering::SeqCst);
            }
            AuditResult::Failed => {
                self.failed.fetch_add(1, Ordering::SeqCst);
            }
        }
        Ok(())
    }
}

fn provenance(resource: &ProviderResourceRef, revision: &str) -> ProviderProvenance {
    ProviderProvenance::new(
        resource.clone(),
        revision_of(revision),
        cortex_domain::ContentHash::new([0_u8; 32]),
    )
}

fn revision_of(value: &str) -> ObservedRevision {
    ObservedRevision::new(value.to_owned()).expect("valid revision")
}

fn resource_from(
    workspace_id: WorkspaceId,
    kind: ProviderResourceKind,
    id: &str,
) -> ProviderResourceRef {
    ProviderResourceRef::new(
        workspace_id,
        ProviderId::new(PROVIDER).expect("valid provider id"),
        cortex_domain::ProviderResourceId::new(id).expect("valid resource id"),
        kind,
    )
}

fn task_id_of(resource: &ProviderResourceRef) -> TaskId {
    let raw = resource.resource_id().as_str().trim_start_matches("task:");
    let uuid = raw.parse::<Uuid>().unwrap_or_else(|_| Uuid::now_v7());
    TaskId::try_from(uuid).expect("valid task id")
}

type Authority =
    ProviderAuthority<GrantPolicy, SharedKnowledge, SharedTasks, FakeOperationLog, SharedAudit>;

struct Harness {
    authority: Authority,
    knowledge: std::sync::Arc<FakeKnowledge>,
    tasks: std::sync::Arc<FakeTasks>,
    audit: std::sync::Arc<RecordingAudit>,
}

fn authority(context: &CommandContext, capabilities: &[Capability]) -> Harness {
    let knowledge = std::sync::Arc::new(FakeKnowledge::default());
    let tasks = std::sync::Arc::new(FakeTasks::default());
    let audit = std::sync::Arc::new(RecordingAudit::default());
    let authority = ProviderAuthority::new(
        granted(context, capabilities.iter().copied()),
        SharedKnowledge(knowledge.clone()),
        SharedTasks(tasks.clone()),
        FakeOperationLog::default(),
        SharedAudit(audit.clone()),
    );
    Harness {
        authority,
        knowledge,
        tasks,
        audit,
    }
}

struct SharedKnowledge(std::sync::Arc<FakeKnowledge>);
impl KnowledgeProvider for SharedKnowledge {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<cortex_application::KnowledgeDocument>>, ProviderError> {
        self.0.get(resource).await
    }
    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<cortex_application::KnowledgeDocument>, ProviderError> {
        self.0.search(query).await
    }
    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        self.0.create(input).await
    }
    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        self.0.update(input).await
    }
    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        self.0.delete(input).await
    }
}

struct SharedTasks(std::sync::Arc<FakeTasks>);
impl TaskProvider for SharedTasks {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        self.0.get(resource).await
    }
    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        self.0.search(query).await
    }
    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        self.0.create(input).await
    }
    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        self.0.update(input).await
    }
    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        self.0.complete(input).await
    }
    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        self.0.delete(input).await
    }
}

struct SharedAudit(std::sync::Arc<RecordingAudit>);
impl AuditPort for SharedAudit {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError> {
        self.0.append(event).await
    }
}

#[tokio::test]
async fn a_granted_task_create_dispatches_records_and_audits_success() {
    let context = context();
    let harness = authority(&context, &[Capability::TaskCreate]);
    let operation_id = OperationId::new();
    let input = TaskCreate::new(
        context.workspace_id,
        operation_id,
        TaskId::new(),
        "ship the cutover",
        String::new(),
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");

    let outcome = harness
        .authority
        .create_task(&context, Capability::TaskCreate, input)
        .await
        .expect("create succeeds");

    assert_eq!(
        outcome.previous_revision, None,
        "a creation has no previous revision"
    );
    assert_eq!(
        outcome
            .current_revision
            .as_ref()
            .map(ObservedRevision::as_str),
        Some("rev-1")
    );
    assert_eq!(harness.tasks.dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(harness.audit.succeeded.load(Ordering::SeqCst), 1);
    assert_eq!(harness.audit.rejected.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replaying_the_same_operation_id_returns_the_recorded_outcome_without_redispatch() {
    let context = context();
    let harness = authority(&context, &[Capability::TaskCreate]);
    let task_id = TaskId::new();
    let request = |context: &CommandContext| {
        TaskCreate::new(
            context.workspace_id,
            context.operation_id,
            task_id,
            "replay me",
            String::new(),
            ProviderTaskPriority::Normal,
            TaskSchedulingMetadata::default(),
        )
        .expect("valid input")
    };

    let first = harness
        .authority
        .create_task(&context, Capability::TaskCreate, request(&context))
        .await
        .expect("first dispatch succeeds");
    let replay = harness
        .authority
        .create_task(&context, Capability::TaskCreate, request(&context))
        .await
        .expect("replay returns the recorded outcome");

    assert_eq!(first, replay);
    assert_eq!(harness.tasks.dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(harness.audit.succeeded.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_same_operation_id_with_a_different_capability_conflicts() {
    let context = context();
    let harness = authority(
        &context,
        &[Capability::TaskCreate, Capability::KnowledgeCreate],
    );
    let task_input = TaskCreate::new(
        context.workspace_id,
        context.operation_id,
        TaskId::new(),
        "identity binding",
        String::new(),
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");
    harness
        .authority
        .create_task(&context, Capability::TaskCreate, task_input)
        .await
        .expect("first dispatch succeeds");

    let knowledge_input = KnowledgeCreate::new(
        context.workspace_id,
        context.operation_id,
        "another capability",
        String::new(),
    )
    .expect("valid input");
    let conflict = harness
        .authority
        .create_knowledge(&context, Capability::KnowledgeCreate, knowledge_input)
        .await
        .expect_err("identity mismatch conflicts");
    assert_eq!(
        conflict,
        ApplicationError::Conflict {
            entity: "operation"
        }
    );
}

#[tokio::test]
async fn a_denied_capability_is_audited_rejected_and_never_dispatched() {
    let context = context();
    let harness = authority(&context, &[]);
    let input = TaskCreate::new(
        context.workspace_id,
        OperationId::new(),
        TaskId::new(),
        "never stored",
        String::new(),
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");

    let denied = harness
        .authority
        .create_task(&context, Capability::TaskCreate, input)
        .await
        .expect_err("denied");

    assert!(matches!(denied, ApplicationError::PolicyDenied(_)));
    assert_eq!(harness.tasks.dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(harness.audit.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(harness.audit.succeeded.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_stale_expected_revision_is_a_typed_conflict_and_the_expected_revision_is_forwarded_untouched()
 {
    let context = context();
    let harness = authority(&context, &[Capability::TaskCreate, Capability::TaskUpdate]);
    let task_id = TaskId::new();
    let create = TaskCreate::new(
        context.workspace_id,
        OperationId::new(),
        task_id,
        "revision gated",
        String::new(),
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");
    let created = harness
        .authority
        .create_task(&context, Capability::TaskCreate, create)
        .await
        .expect("create succeeds");
    let context = next_operation(&context);
    let resource = created.resource;

    // The caller's expected revision is forwarded verbatim: a stale revision
    // is rejected by the provider as a typed conflict.
    let stale = TaskUpdate::new(
        resource.clone(),
        OperationId::new(),
        revision("rev-999"),
        "stale write",
        String::new(),
        ProviderTaskStatus::Todo,
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");
    let conflict = harness
        .authority
        .update_task(&context, Capability::TaskUpdate, stale)
        .await
        .expect_err("stale revision conflicts");
    let context = next_operation(&context);
    assert_eq!(conflict, ApplicationError::Conflict { entity: "task" });
    assert_eq!(harness.audit.rejected.load(Ordering::SeqCst), 1);

    // The fresh expected revision from the create outcome passes through and
    // the mutation succeeds.
    let fresh = TaskUpdate::new(
        resource,
        OperationId::new(),
        created
            .current_revision
            .expect("creation recorded a revision"),
        "fresh write",
        String::new(),
        ProviderTaskStatus::InProgress,
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .expect("valid input");
    let outcome = harness
        .authority
        .update_task(&context, Capability::TaskUpdate, fresh)
        .await
        .expect("fresh revision updates");
    assert_eq!(
        outcome
            .previous_revision
            .as_ref()
            .map(ObservedRevision::as_str),
        Some("rev-1")
    );
    assert_eq!(
        outcome
            .current_revision
            .as_ref()
            .map(ObservedRevision::as_str),
        Some("rev-2")
    );
}

#[tokio::test]
async fn queries_require_their_capability_and_provider_missing_resources_are_typed_not_found() {
    let context = context();
    let unprivileged = authority(&context, &[]);
    let query = TaskQuery::new(
        context.workspace_id,
        None::<String>,
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("valid query");
    let denied = unprivileged
        .authority
        .list_tasks(&context, query)
        .await
        .expect_err("denied without a grant");
    assert!(matches!(denied, ApplicationError::PolicyDenied(_)));

    let harness = authority(&context, &[Capability::TaskDelete]);
    let unknown = resource_from(
        context.workspace_id,
        ProviderResourceKind::Task,
        "task:missing",
    );
    let input = TaskDelete::new(unknown.clone(), OperationId::new(), revision("rev-1"))
        .expect("valid input");
    let missing = harness
        .authority
        .delete_task(&context, Capability::TaskDelete, input)
        .await
        .expect_err("unknown task is typed not-found");
    assert_eq!(missing, ApplicationError::NotFound { entity: "task" });
}

#[tokio::test]
async fn knowledge_create_update_and_delete_run_through_the_same_authority() {
    let context = context();
    let harness = authority(
        &context,
        &[
            Capability::KnowledgeCreate,
            Capability::KnowledgeUpdate,
            Capability::KnowledgeDelete,
        ],
    );

    let created = harness
        .authority
        .create_knowledge(
            &context,
            Capability::KnowledgeCreate,
            KnowledgeCreate::new(
                context.workspace_id,
                OperationId::new(),
                "architecture notes",
                "the vault is authoritative",
            )
            .expect("valid input"),
        )
        .await
        .expect("create succeeds");
    let context = next_operation(&context);

    let updated = harness
        .authority
        .update_knowledge(
            &context,
            Capability::KnowledgeUpdate,
            KnowledgeUpdate::new(
                created.resource.clone(),
                OperationId::new(),
                created.current_revision.clone().expect("revision"),
                "architecture notes",
                "updated body",
            )
            .expect("valid input"),
        )
        .await
        .expect("update succeeds");
    let context = next_operation(&context);

    let deleted = harness
        .authority
        .delete_knowledge(
            &context,
            Capability::KnowledgeDelete,
            KnowledgeDelete::new(
                created.resource,
                OperationId::new(),
                updated.current_revision.expect("revision"),
            )
            .expect("valid input"),
        )
        .await
        .expect("delete succeeds");
    assert_eq!(deleted.current_revision, None);
    assert_eq!(harness.knowledge.dispatches.load(Ordering::SeqCst), 3);
    assert_eq!(harness.audit.succeeded.load(Ordering::SeqCst), 3);
}
