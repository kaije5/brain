use std::num::NonZeroUsize;

use cortex_application::{
    AggregateChange, ApplicationError, AtomicMutation, AtomicMutationPort, Capability,
    CommandContext, Embedding, EntityKind, MutationResult, SearchIndex,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Note, NoteInput, OperationId, PolicyDecision,
    PrincipalId, WorkspaceId,
};
use cortex_search::{VectorRecord, cosine_candidates};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn embedding_blobs_are_little_endian_and_reject_mismatched_dimensions() -> Result<(), String> {
    let embedding = Embedding::new("nomic", "1", vec![1.0, -2.5]).map_err(debug_error)?;
    let bytes = embedding.to_le_bytes();

    assert_eq!(bytes, [0, 0, 128, 63, 0, 0, 32, 192]);
    assert_eq!(
        Embedding::from_le_bytes("nomic", "1", 2, &bytes).map_err(debug_error)?,
        embedding
    );
    assert_eq!(
        Embedding::from_le_bytes("nomic", "1", 3, &bytes),
        Err(ApplicationError::Validation {
            field: "embedding_dimensions"
        })
    );
    Ok(())
}

#[test]
fn cosine_candidates_scan_only_the_configured_bound() -> Result<(), String> {
    let query = embedding(vec![1.0, 0.0])?;
    let a = id("01900000-0000-7000-8000-000000000001")?;
    let b = id("01900000-0000-7000-8000-000000000002")?;
    let c = id("01900000-0000-7000-8000-000000000003")?;
    let records = vec![
        VectorRecord::new(a, embedding(vec![0.8, 0.2])?),
        VectorRecord::new(b, embedding(vec![0.7, 0.3])?),
        VectorRecord::new(c, embedding(vec![1.0, 0.0])?),
    ];

    let candidates = cosine_candidates(
        &query,
        records,
        NonZeroUsize::new(2).ok_or("non-zero bound required")?,
    )
    .map_err(debug_error)?;

    assert_eq!(
        candidates
            .into_iter()
            .map(|candidate| candidate.entity_id)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
    Ok(())
}

#[test]
fn cosine_candidates_reject_record_dimensions_that_do_not_match_query() -> Result<(), String> {
    let query = embedding(vec![1.0, 0.0])?;
    let records = vec![VectorRecord::new(
        id("01900000-0000-7000-8000-000000000001")?,
        embedding(vec![1.0, 0.0, 0.0])?,
    )];

    assert_eq!(
        cosine_candidates(
            &query,
            records,
            NonZeroUsize::new(1).ok_or("non-zero bound required")?
        ),
        Err(ApplicationError::Validation {
            field: "embedding_dimensions"
        })
    );
    Ok(())
}

#[tokio::test]
async fn sqlite_embeddings_validate_blob_dimensions_and_load_bounded_authorized_records()
-> Result<(), String> {
    let temp = TempDir::new().map_err(debug_error)?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let repositories = database.repositories();
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    repositories
        .create_workspace(workspace_id, "workspace")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "principal")
        .await
        .map_err(debug_error)?;
    repositories
        .grant_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
        .await
        .map_err(debug_error)?;
    let note = Note::create(NoteInput {
        workspace_id,
        title: "Local inference".to_owned(),
        content: "Cortex uses local embeddings".to_owned(),
    })
    .map_err(debug_error)?;
    persist_note(&database, principal_id, note.clone()).await?;
    repositories
        .upsert_search_document(workspace_id, note.id(), EntityKind::Note, note.content())
        .await
        .map_err(debug_error)?;
    let stored = embedding(vec![0.8, 0.2])?;
    assert_eq!(
        repositories
            .upsert_embedding(
                workspace_id,
                note.id(),
                stored.model_id(),
                stored.model_version(),
                3,
                &stored.to_le_bytes(),
            )
            .await,
        Err(ApplicationError::Validation {
            field: "embedding_dimensions"
        })
    );
    for vector in [
        f32::NAN.to_le_bytes().to_vec(),
        f32::INFINITY.to_le_bytes().to_vec(),
        [0.0_f32.to_le_bytes(), (-0.0_f32).to_le_bytes()].concat(),
    ] {
        assert_eq!(
            repositories
                .upsert_embedding(
                    workspace_id,
                    note.id(),
                    stored.model_id(),
                    stored.model_version(),
                    vector.len() / size_of::<f32>(),
                    &vector,
                )
                .await,
            Err(ApplicationError::Validation {
                field: "embedding_vector"
            })
        );
    }
    let query = embedding(vec![1.0, 0.0])?;
    assert!(
        semantic_records(&repositories, workspace_id, principal_id, &query)
            .await?
            .is_empty()
    );
    repositories
        .upsert_embedding(
            workspace_id,
            note.id(),
            stored.model_id(),
            stored.model_version(),
            stored.dimensions(),
            &stored.to_le_bytes(),
        )
        .await
        .map_err(debug_error)?;

    let records = SearchIndex::semantic_records(
        &repositories,
        workspace_id,
        principal_id,
        &query,
        NonZeroUsize::new(1).ok_or("non-zero bound required")?,
    )
    .await
    .map_err(debug_error)?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].candidate.entity_id, note.id());
    assert_eq!(records[0].candidate.kind, EntityKind::Note);
    assert_eq!(records[0].embedding, stored);
    Ok(())
}

#[tokio::test]
async fn changing_searchable_text_atomically_invalidates_the_stale_embedding() -> Result<(), String>
{
    let temp = TempDir::new().map_err(debug_error)?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let repositories = database.repositories();
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    repositories
        .create_workspace(workspace_id, "workspace")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "principal")
        .await
        .map_err(debug_error)?;
    repositories
        .grant_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
        .await
        .map_err(debug_error)?;
    let note = Note::create(NoteInput {
        workspace_id,
        title: "Content hash".to_owned(),
        content: "content A".to_owned(),
    })
    .map_err(debug_error)?;
    persist_note(&database, principal_id, note.clone()).await?;
    repositories
        .upsert_search_document(workspace_id, note.id(), EntityKind::Note, "content A")
        .await
        .map_err(debug_error)?;
    let old_embedding = embedding(vec![1.0, 0.0])?;
    repositories
        .upsert_embedding(
            workspace_id,
            note.id(),
            old_embedding.model_id(),
            old_embedding.model_version(),
            old_embedding.dimensions(),
            &old_embedding.to_le_bytes(),
        )
        .await
        .map_err(debug_error)?;
    assert_eq!(
        semantic_records(&repositories, workspace_id, principal_id, &old_embedding)
            .await?
            .len(),
        1
    );

    repositories
        .upsert_search_document(workspace_id, note.id(), EntityKind::Note, "content B")
        .await
        .map_err(debug_error)?;
    assert!(
        semantic_records(&repositories, workspace_id, principal_id, &old_embedding)
            .await?
            .is_empty()
    );

    let new_embedding = embedding(vec![0.0, 1.0])?;
    repositories
        .upsert_embedding(
            workspace_id,
            note.id(),
            new_embedding.model_id(),
            new_embedding.model_version(),
            new_embedding.dimensions(),
            &new_embedding.to_le_bytes(),
        )
        .await
        .map_err(debug_error)?;
    let records =
        semantic_records(&repositories, workspace_id, principal_id, &new_embedding).await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].candidate.snippet, "content B");
    assert_eq!(records[0].embedding, new_embedding);
    Ok(())
}

async fn semantic_records(
    repositories: &cortex_storage::SqliteRepositories,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    query: &Embedding,
) -> Result<Vec<cortex_application::IndexedVector>, String> {
    SearchIndex::semantic_records(
        repositories,
        workspace_id,
        principal_id,
        query,
        NonZeroUsize::new(10).ok_or("non-zero bound required")?,
    )
    .await
    .map_err(debug_error)
}

async fn persist_note(
    database: &SqliteDatabase,
    principal_id: PrincipalId,
    note: Note,
) -> Result<(), String> {
    let context = CommandContext::from_authenticated(
        note.workspace_id(),
        principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    );
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let mutation = AtomicMutation::new(
        context,
        Capability::NoteCreate,
        None,
        vec![AggregateChange::InsertNote(note.clone())],
        result,
        AuditEvent {
            id: AuditEventId::new(),
            workspace_id: note.workspace_id(),
            principal_id,
            operation_id: context.operation_id,
            correlation_id: context.correlation_id,
            capability: Capability::NoteCreate.metadata().mcp_name,
            target: Some(cortex_domain::ResourceTarget::CortexEntity(note.id())),
            provider_metadata: None,
            policy_decision: PolicyDecision::Allow,
            result: AuditResult::Succeeded,
        },
    )
    .map_err(debug_error)?;
    database
        .operation_store()
        .execute_once(mutation)
        .await
        .map(|_| ())
        .map_err(debug_error)
}

fn embedding(values: Vec<f32>) -> Result<Embedding, String> {
    Embedding::new("nomic", "1", values).map_err(debug_error)
}

fn id(value: &str) -> Result<EntityId, String> {
    let uuid = Uuid::parse_str(value).map_err(|error| error.to_string())?;
    EntityId::try_from(uuid).map_err(debug_error)
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
