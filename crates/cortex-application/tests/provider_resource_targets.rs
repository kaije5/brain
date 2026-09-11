mod support;

use cortex_application::{
    ApplicationError, Capability, CapabilityGrant, CommandContext, GrantPolicy, NoteCreateInput,
    NoteUpdateInput, PolicyDecision, PolicyPort,
};
use cortex_domain::{
    AuditResult, OperationId, PolicyDeny, ProviderId, ProviderResourceId, ProviderResourceKind,
    ProviderResourceRef, ResourceTarget, WorkspaceId,
};
use uuid::Uuid;

use support::{Fixture, debug_error};

fn context(workspace_id: WorkspaceId, principal_id: cortex_domain::PrincipalId) -> CommandContext {
    CommandContext::from_authenticated(
        workspace_id,
        principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    )
}

#[test]
fn grant_policy_denies_provider_targets_outside_the_authenticated_workspace() {
    let workspace_id = WorkspaceId::new();
    let principal_id = cortex_domain::PrincipalId::new();
    let policy = GrantPolicy::new([CapabilityGrant::new(
        workspace_id,
        principal_id,
        Capability::NoteSearch,
    )]);
    let context = context(workspace_id, principal_id);
    let provider_id = ProviderId::new("primary-vault").unwrap();

    let local_resource = ProviderResourceRef::new(
        workspace_id,
        provider_id.clone(),
        ProviderResourceId::new("01K4RESOURCE").unwrap(),
        ProviderResourceKind::Knowledge,
    );
    assert_eq!(
        GrantPolicy::evaluate(
            &policy,
            &context,
            Capability::NoteSearch,
            Some(&ResourceTarget::ProviderResource(local_resource)),
        ),
        PolicyDecision::Allow
    );

    let foreign_resource = ProviderResourceRef::new(
        WorkspaceId::new(),
        provider_id.clone(),
        ProviderResourceId::new("01K4FOREIGN").unwrap(),
        ProviderResourceKind::Knowledge,
    );
    assert_eq!(
        GrantPolicy::evaluate(
            &policy,
            &context,
            Capability::NoteSearch,
            Some(&ResourceTarget::ProviderResource(foreign_resource)),
        ),
        PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace)
    );

    let foreign_scope = ResourceTarget::ProviderScope {
        provider_id,
        workspace_id: WorkspaceId::new(),
        resource_kind: ProviderResourceKind::Task,
    };
    assert_eq!(
        GrantPolicy::evaluate(
            &policy,
            &context,
            Capability::NoteCreate,
            Some(&foreign_scope),
        ),
        PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace)
    );
}

#[tokio::test]
async fn replaying_an_operation_against_a_different_target_conflicts() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let first = fixture
        .service
        .create_note(
            fixture.context(),
            NoteCreateInput {
                title: "First".to_owned(),
                content: "first body".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let second = fixture
        .service
        .create_note(
            fixture.context(),
            NoteCreateInput {
                title: "Second".to_owned(),
                content: "second body".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;

    // Record operation `update_operation` against the first note.
    let update_operation = OperationId::new();
    fixture
        .service
        .update_note(
            fixture.context_for(update_operation),
            first.entity_id,
            first.revision,
            NoteUpdateInput {
                title: "First".to_owned(),
                content: "first body updated".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;

    // The same operation ID against a different target must conflict, not
    // return the recorded result or mutate the second note.
    let conflicted = fixture
        .service
        .update_note(
            fixture.context_for(update_operation),
            second.entity_id,
            second.revision,
            NoteUpdateInput {
                title: "Second".to_owned(),
                content: "second body updated".to_owned(),
            },
        )
        .await;
    assert!(
        matches!(
            conflicted,
            Err(ApplicationError::Conflict {
                entity: "operation"
            })
        ),
        "expected operation conflict, got {conflicted:?}"
    );
    Ok(())
}

#[tokio::test]
async fn provider_audit_targets_are_recorded_without_content() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let created = fixture
        .service
        .create_note(
            fixture.context(),
            NoteCreateInput {
                title: "Secret Title".to_owned(),
                content: "secret body".to_owned(),
            },
        )
        .await
        .map_err(debug_error)?;
    let deleted = fixture
        .service
        .delete_note(fixture.context(), created.entity_id, created.revision)
        .await
        .map_err(debug_error)?;
    assert_eq!(deleted.lifecycle, cortex_domain::Lifecycle::Deleted);

    let audits = fixture.state.audits()?;
    let last = audits.last().ok_or("missing delete audit")?;
    assert_eq!(last.result, AuditResult::Succeeded);
    assert_eq!(
        last.target,
        Some(ResourceTarget::CortexEntity(created.entity_id))
    );
    assert_eq!(last.provider_metadata, None);
    let rendered = format!("{audits:?}");
    assert!(!rendered.contains("Secret Title"));
    assert!(!rendered.contains("secret body"));
    Ok(())
}
