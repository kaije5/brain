//! Durable provider operation idempotency log round trip (SCRUM-116).
//!
//! The store is exercised through the full provider authority so the record
//! round trip covers the exact shape the application layer persists.

use cortex_application::{
    Capability, CapabilityGrant, CommandContext, GrantPolicy, ProviderAuthority, ProviderMutation,
    ProviderOperationLog, ProviderTaskPriority, TaskCreate, TaskProvider, TaskSchedulingMetadata,
};
use cortex_domain::{
    ContentHash, ObservedRevision, OperationId, PrincipalId, ProviderId, ProviderProvenance,
    ProviderResourceId, ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn provider_operation_records_round_trip_and_replay() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let store = database.provider_operation_store();

    let context = CommandContext::from_authenticated(
        WorkspaceId::new(),
        PrincipalId::new(),
        OperationId::new(),
        Uuid::now_v7(),
    );
    let policy = GrantPolicy::new([CapabilityGrant::new(
        context.workspace_id,
        context.principal_id,
        Capability::TaskCreate,
    )]);
    let authority = ProviderAuthority::new(
        policy,
        MissingKnowledge,
        RecordingTasks::default(),
        store.clone(),
        NullAudit,
    );

    let input = TaskCreate::new(
        context.workspace_id,
        context.operation_id,
        TaskId::new(),
        "round trip",
        String::new(),
        ProviderTaskPriority::Normal,
        TaskSchedulingMetadata::default(),
    )
    .map_err(debug_error)?;

    let outcome = authority
        .create_task(&context, Capability::TaskCreate, input)
        .await
        .map_err(debug_error)?;
    assert_eq!(
        outcome
            .current_revision
            .as_ref()
            .map(ObservedRevision::as_str),
        Some("rev-1")
    );

    // The durable log replays the outcome with the exact command identity.
    let replay = store
        .find(context.workspace_id, context.operation_id)
        .await
        .map_err(debug_error)?
        .expect("provider operation is recorded");
    assert_eq!(replay.outcome, outcome);
    assert_eq!(
        replay.identity.principal_id, context.principal_id,
        "the recorded principal round trips exactly"
    );
    // Creates address a scope, not an existing resource, so the identity
    // target is empty; revisions and resource identity live in the outcome.
    assert_eq!(replay.identity.target, None);
    Ok(())
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

// -- minimal provider doubles ------------------------------------------------

#[derive(Default)]
struct RecordingTasks {
    creations: std::sync::atomic::AtomicUsize,
}

impl TaskProvider for RecordingTasks {
    async fn get(
        &self,
        _resource: &ProviderResourceRef,
    ) -> Result<
        Option<cortex_application::ProviderRead<cortex_application::ProviderTask>>,
        cortex_application::ProviderError,
    > {
        Ok(None)
    }

    async fn search(
        &self,
        _query: &cortex_application::TaskQuery,
    ) -> Result<
        cortex_application::ProviderPage<cortex_application::ProviderTask>,
        cortex_application::ProviderError,
    > {
        cortex_application::ProviderPage::new(
            Vec::new(),
            cortex_application::ProviderFreshness::Current,
        )
    }

    async fn create(
        &self,
        input: TaskCreate,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        self.creations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let resource = ProviderResourceRef::new(
            input.workspace_id(),
            ProviderId::new("fake-vault").expect("valid provider id"),
            ProviderResourceId::new(format!("task:{}", Uuid::from(input.task_id())))
                .expect("valid resource id"),
            ProviderResourceKind::Task,
        );
        Ok(ProviderMutation::created(ProviderProvenance::new(
            resource,
            ObservedRevision::new("rev-1").expect("valid revision"),
            ContentHash::new([0_u8; 32]),
        )))
    }

    async fn update(
        &self,
        _input: cortex_application::TaskUpdate,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }

    async fn complete(
        &self,
        _input: cortex_application::TaskComplete,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }

    async fn delete(
        &self,
        _input: cortex_application::TaskDelete,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }
}

struct MissingKnowledge;

impl cortex_application::KnowledgeProvider for MissingKnowledge {
    async fn get(
        &self,
        _resource: &ProviderResourceRef,
    ) -> Result<
        Option<cortex_application::ProviderRead<cortex_application::KnowledgeDocument>>,
        cortex_application::ProviderError,
    > {
        Ok(None)
    }

    async fn search(
        &self,
        _query: &cortex_application::KnowledgeQuery,
    ) -> Result<
        cortex_application::ProviderPage<cortex_application::KnowledgeDocument>,
        cortex_application::ProviderError,
    > {
        cortex_application::ProviderPage::new(
            Vec::new(),
            cortex_application::ProviderFreshness::Current,
        )
    }

    async fn create(
        &self,
        _input: cortex_application::KnowledgeCreate,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }

    async fn update(
        &self,
        _input: cortex_application::KnowledgeUpdate,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }

    async fn delete(
        &self,
        _input: cortex_application::KnowledgeDelete,
    ) -> Result<ProviderMutation, cortex_application::ProviderError> {
        Err(cortex_application::ProviderError::NotFound {
            resource: missing_resource(),
        })
    }
}

struct NullAudit;

impl cortex_application::AuditPort for NullAudit {
    async fn append(
        &self,
        _event: cortex_domain::AuditEvent,
    ) -> Result<(), cortex_application::ApplicationError> {
        Ok(())
    }
}

fn missing_resource() -> ProviderResourceRef {
    ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("fake-vault").expect("valid provider id"),
        ProviderResourceId::new("missing").expect("valid resource id"),
        ProviderResourceKind::Task,
    )
}
