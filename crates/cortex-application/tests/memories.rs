mod support;

use cortex_application::{
    ApplicationError, MemoryCorrectInput, MemoryCreateInput, MemoryRepository,
};
use cortex_domain::{AuditResult, Lifecycle, MemoryStatus, SourceRef};

use support::{Fixture, debug_error};

#[tokio::test]
async fn correction_atomically_supersedes_predecessor_and_creates_sourced_successor()
-> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let source_id = fixture.seed_source("explicit user statement")?;
    let created = fixture
        .service
        .create_memory(fixture.context(), memory_create(source_id, "OldModel"))
        .await
        .map_err(debug_error)?;
    let corrected = fixture
        .service
        .correct_memory(
            fixture.context(),
            created.entity_id,
            created.revision,
            MemoryCorrectInput {
                statement: "Cortex uses Nemotron".to_owned(),
                normalized_subject: "cortex".to_owned(),
                normalized_predicate: "uses_local_ai".to_owned(),
                normalized_object: "nemotron".to_owned(),
                sources: vec![SourceRef { source_id }],
            },
        )
        .await
        .map_err(debug_error)?;

    let predecessor =
        MemoryRepository::find_history(&fixture.state, fixture.workspace_id, created.entity_id)
            .await
            .map_err(debug_error)?
            .ok_or("missing predecessor")?;
    assert_eq!(predecessor.status(), MemoryStatus::Superseded);
    assert_eq!(
        MemoryRepository::find(&fixture.state, fixture.workspace_id, created.entity_id)
            .await
            .map_err(debug_error)?,
        None
    );
    let successor =
        MemoryRepository::find(&fixture.state, fixture.workspace_id, corrected.entity_id)
            .await
            .map_err(debug_error)?
            .ok_or("missing successor")?;
    assert_eq!(successor.supersedes(), Some(created.entity_id));
    assert_eq!(successor.sources(), &[SourceRef { source_id }]);
    assert_eq!(fixture.state.audits()?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn deleted_memory_is_hidden_until_restored() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let source_id = fixture.seed_source("explicit user statement")?;
    let created = fixture
        .service
        .create_memory(fixture.context(), memory_create(source_id, "Nemotron"))
        .await
        .map_err(debug_error)?;
    let deleted = fixture
        .service
        .delete_memory(fixture.context(), created.entity_id, created.revision)
        .await
        .map_err(debug_error)?;
    assert_eq!(deleted.lifecycle, Lifecycle::Deleted);
    assert_eq!(
        MemoryRepository::find(&fixture.state, fixture.workspace_id, deleted.entity_id)
            .await
            .map_err(debug_error)?,
        None
    );
    let restored = fixture
        .service
        .restore_memory(fixture.context(), deleted.entity_id, deleted.revision)
        .await
        .map_err(debug_error)?;
    assert_eq!(restored.lifecycle, Lifecycle::Active);
    assert!(
        MemoryRepository::find(&fixture.state, fixture.workspace_id, restored.entity_id)
            .await
            .map_err(debug_error)?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn memory_creation_rejects_missing_workspace_provenance_and_audits_the_decision()
-> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let result = fixture
        .service
        .create_memory(
            fixture.context(),
            memory_create(cortex_domain::EntityId::new(), "Unsupported"),
        )
        .await;

    assert_eq!(result, Err(ApplicationError::NotFound { entity: "source" }));
    assert_eq!(fixture.state.audits()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn memory_creation_rejects_tombstoned_provenance_without_mutation() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let source_id = fixture.seed_deleted_source("deleted evidence")?;

    let result = fixture
        .service
        .create_memory(fixture.context(), memory_create(source_id, "Unsupported"))
        .await;

    assert_eq!(result, Err(ApplicationError::NotFound { entity: "source" }));
    assert_eq!(fixture.state.memory_count()?, 0);
    let audits = fixture.state.audits()?;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].result, AuditResult::Rejected);
    Ok(())
}

#[tokio::test]
async fn memory_correction_rejects_tombstoned_provenance_without_mutation() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let active_source = fixture.seed_source("active evidence")?;
    let created = fixture
        .service
        .create_memory(fixture.context(), memory_create(active_source, "OldModel"))
        .await
        .map_err(debug_error)?;
    let deleted_source = fixture.seed_deleted_source("deleted correction evidence")?;

    let result = fixture
        .service
        .correct_memory(
            fixture.context(),
            created.entity_id,
            created.revision,
            MemoryCorrectInput {
                statement: "Cortex uses Unsupported".to_owned(),
                normalized_subject: "cortex".to_owned(),
                normalized_predicate: "uses_local_ai".to_owned(),
                normalized_object: "unsupported".to_owned(),
                sources: vec![SourceRef {
                    source_id: deleted_source,
                }],
            },
        )
        .await;

    assert_eq!(result, Err(ApplicationError::NotFound { entity: "source" }));
    assert_eq!(fixture.state.memory_count()?, 1);
    let predecessor =
        MemoryRepository::find(&fixture.state, fixture.workspace_id, created.entity_id)
            .await
            .map_err(debug_error)?
            .ok_or("predecessor was mutated")?;
    assert_eq!(predecessor.status(), MemoryStatus::Active);
    assert_eq!(predecessor.revision(), created.revision);
    let audits = fixture.state.audits()?;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[1].result, AuditResult::Rejected);
    Ok(())
}

fn memory_create(source_id: cortex_domain::EntityId, object: &str) -> MemoryCreateInput {
    MemoryCreateInput {
        statement: format!("Cortex uses {object}"),
        normalized_subject: "cortex".to_owned(),
        normalized_predicate: "uses_local_ai".to_owned(),
        normalized_object: object.to_ascii_lowercase(),
        sources: vec![SourceRef { source_id }],
    }
}
