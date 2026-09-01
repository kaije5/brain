mod support;

use cortex_application::{
    ApplicationError, ApplicationService, Capability, CapabilityGrant, CommandContext, GrantPolicy,
    NoteCreateInput, NoteRepository, NoteUpdateInput, TaskCreateInput,
};
use cortex_domain::{Lifecycle, OperationId, PrincipalId};
use uuid::Uuid;

use support::{Fixture, debug_error};

#[tokio::test]
async fn stale_delete_is_rejected_and_leaves_newer_note_active() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let note = fixture
        .service
        .create_note(fixture.context(), new_note())
        .await
        .map_err(debug_error)?;
    let updated = fixture
        .service
        .update_note(
            fixture.context(),
            note.entity_id,
            note.revision,
            NoteUpdateInput {
                title: "Updated".to_owned(),
                content: "newer content".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let result = fixture
        .service
        .delete_note(fixture.context(), note.entity_id, note.revision)
        .await;

    assert!(matches!(
        result,
        Err(ApplicationError::Conflict { entity: "note" })
    ));
    let persisted = NoteRepository::find(&fixture.state, fixture.workspace_id, note.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("newer note missing")?;
    assert_eq!(persisted.revision(), updated.revision);
    assert_eq!(persisted.lifecycle(), Lifecycle::Active);
    assert_eq!(persisted.content(), "newer content");
    assert_eq!(fixture.state.audits()?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn repeated_operation_is_returned_before_revision_validation() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let note = fixture
        .service
        .create_note(fixture.context(), new_note())
        .await
        .map_err(debug_error)?;
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .update_note(
            fixture.context_for(operation_id),
            note.entity_id,
            note.revision,
            NoteUpdateInput {
                title: "First".to_owned(),
                content: "persisted once".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let replay = fixture
        .service
        .update_note(
            fixture.context_for(operation_id),
            note.entity_id,
            note.revision,
            NoteUpdateInput {
                title: "Ignored replay".to_owned(),
                content: "must not replace".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;

    assert_eq!(replay, first);
    let persisted = NoteRepository::find(&fixture.state, fixture.workspace_id, note.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("note missing")?;
    assert_eq!(persisted.title(), "First");
    assert_eq!(fixture.state.audits()?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn repeated_operation_still_requires_the_current_capability() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .create_note(fixture.context_for(operation_id), new_note())
        .await
        .map_err(debug_error)?;
    let denied = ApplicationService::new(
        GrantPolicy::new([]),
        fixture.state.clone(),
        fixture.state.clone(),
        fixture.state.clone(),
    );

    let replay = denied
        .update_note(
            fixture.context_for(operation_id),
            first.entity_id,
            first.revision,
            NoteUpdateInput {
                title: "Unauthorized replay".to_owned(),
                content: "must not bypass policy".to_owned(),
            },
        )
        .await;

    assert!(matches!(replay, Err(ApplicationError::PolicyDenied(_))));
    let persisted = NoteRepository::find(&fixture.state, fixture.workspace_id, first.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("note missing")?;
    assert_eq!(persisted.title(), "Original");
    assert_eq!(fixture.state.audits()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_granted_principal() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    let first = fixture
        .service
        .create_note(fixture.context_for(operation_id), new_note())
        .await
        .map_err(debug_error)?;
    let other_principal = PrincipalId::new();
    let other_service = ApplicationService::new(
        GrantPolicy::new([CapabilityGrant::new(
            fixture.workspace_id,
            other_principal,
            Capability::NoteCreate,
        )]),
        fixture.state.clone(),
        fixture.state.clone(),
        fixture.state.clone(),
    );

    let replay = other_service
        .create_note(
            CommandContext::from_authenticated(
                fixture.workspace_id,
                other_principal,
                operation_id,
                Uuid::now_v7(),
            ),
            NoteCreateInput {
                title: "other principal".to_owned(),
                content: "must not receive the first result".to_owned(),
            },
        )
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    assert_eq!(fixture.state.audits()?.len(), 1);
    let persisted = NoteRepository::find(&fixture.state, fixture.workspace_id, first.entity_id)
        .await
        .map_err(debug_error)?;
    assert!(persisted.is_some());
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_allowed_capability() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let operation_id = OperationId::new();
    fixture
        .service
        .create_note(fixture.context_for(operation_id), new_note())
        .await
        .map_err(debug_error)?;

    let replay = fixture
        .service
        .create_task(
            fixture.context_for(operation_id),
            TaskCreateInput {
                title: "must not be acknowledged".to_owned(),
                due_at: None,
            },
        )
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    assert_eq!(fixture.state.audits()?.len(), 1);
    assert_eq!(fixture.state.task_count()?, 0);
    Ok(())
}

#[tokio::test]
async fn repeated_operation_rejects_a_different_target() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let first_note = fixture
        .service
        .create_note(fixture.context(), new_note())
        .await
        .map_err(debug_error)?;
    let second_note = fixture
        .service
        .create_note(fixture.context(), new_note())
        .await
        .map_err(debug_error)?;
    let operation_id = OperationId::new();
    fixture
        .service
        .update_note(
            fixture.context_for(operation_id),
            first_note.entity_id,
            first_note.revision,
            NoteUpdateInput {
                title: "first target".to_owned(),
                content: "updated once".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;

    let replay = fixture
        .service
        .update_note(
            fixture.context_for(operation_id),
            second_note.entity_id,
            second_note.revision,
            NoteUpdateInput {
                title: "second target".to_owned(),
                content: "must remain unchanged".to_owned(),
            },
        )
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    let second = NoteRepository::find(&fixture.state, fixture.workspace_id, second_note.entity_id)
        .await
        .map_err(debug_error)?
        .ok_or("second note missing")?;
    assert_eq!(second.title(), "Original");
    assert_eq!(fixture.state.audits()?.len(), 3);
    Ok(())
}

fn new_note() -> NoteCreateInput {
    NoteCreateInput {
        title: "Original".to_owned(),
        content: "original content".to_owned(),
    }
}
