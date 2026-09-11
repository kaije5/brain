mod support;

use cortex_application::{Capability, NoteCreateInput, NoteRepository, NoteUpdateInput};
use cortex_domain::{AuditResult, Lifecycle, PolicyDecision};

use support::{Fixture, debug_error};

#[tokio::test]
async fn note_commands_are_audited_and_tombstones_require_explicit_history() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let created = fixture
        .service
        .create_note(
            fixture.context(),
            NoteCreateInput {
                title: "Architecture".to_owned(),
                content: "Cortex owns canonical state".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let updated = fixture
        .service
        .update_note(
            fixture.context(),
            created.entity_id,
            created.revision,
            NoteUpdateInput {
                title: "Architecture".to_owned(),
                content: "Cortex owns canonical audited state".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let deleted = fixture
        .service
        .delete_note(fixture.context(), updated.entity_id, updated.revision)
        .await
        .map_err(debug_error)?;

    assert_eq!(deleted.lifecycle, Lifecycle::Deleted);
    assert_eq!(
        NoteRepository::find(&fixture.state, fixture.workspace_id, deleted.entity_id)
            .await
            .map_err(debug_error)?,
        None
    );
    let tombstone =
        NoteRepository::find_history(&fixture.state, fixture.workspace_id, deleted.entity_id)
            .await
            .map_err(debug_error)?
            .ok_or("missing tombstone")?;
    assert_eq!(tombstone.lifecycle(), Lifecycle::Deleted);

    let restored = fixture
        .service
        .restore_note(fixture.context(), deleted.entity_id, deleted.revision)
        .await
        .map_err(debug_error)?;
    assert_eq!(restored.lifecycle, Lifecycle::Active);
    assert!(
        NoteRepository::find(&fixture.state, fixture.workspace_id, restored.entity_id)
            .await
            .map_err(debug_error)?
            .is_some()
    );
    assert_eq!(fixture.state.audits()?.len(), 4);
    Ok(())
}

#[tokio::test]
async fn denied_note_create_is_rejected_audited_and_has_no_side_effect() -> Result<(), String> {
    let fixture = Fixture::with_capabilities([]);
    let result = fixture
        .service
        .create_note(
            fixture.context(),
            NoteCreateInput {
                title: "Denied".to_owned(),
                content: "must not persist".to_owned(),
            },
        )
        .await;

    assert!(matches!(
        result,
        Err(cortex_application::ApplicationError::PolicyDenied(_))
    ));
    let audits = fixture.state.audits()?;
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].capability,
        Capability::NoteCreate.metadata().mcp_name
    );
    assert_eq!(
        audits[0].policy_decision,
        PolicyDecision::Deny(cortex_domain::PolicyDeny::MissingGrant)
    );
    assert_eq!(audits[0].result, AuditResult::Rejected);
    assert_eq!(audits[0].target, None);
    Ok(())
}
