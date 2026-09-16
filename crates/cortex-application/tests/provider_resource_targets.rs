mod support;

use cortex_application::{
    ApplicationError, Capability, CapabilityGrant, CommandContext, GrantPolicy, MemoryCreateInput,
    PolicyDecision, PolicyPort,
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
        Capability::MemorySearch,
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
            Capability::MemorySearch,
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
            Capability::MemorySearch,
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
            Capability::MemoryCreate,
            Some(&foreign_scope),
        ),
        PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace)
    );
}

#[tokio::test]
async fn replaying_an_operation_against_a_different_target_conflicts() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let source_id = fixture.seed_source("seed")?;
    let memory_input = MemoryCreateInput {
        statement: "First".to_owned(),
        normalized_subject: "subject".to_owned(),
        normalized_predicate: "predicate".to_owned(),
        normalized_object: "object".to_owned(),
        sources: vec![cortex_domain::SourceRef { source_id }],
    };
    let first = fixture
        .service
        .create_memory(fixture.context(), memory_input.clone())
        .await
        .map_err(debug_error)?;
    let second = fixture
        .service
        .create_memory(fixture.context(), memory_input)
        .await
        .map_err(debug_error)?;

    // Record operation `update_operation` against the first memory.
    let update_operation = OperationId::new();
    fixture
        .service
        .correct_memory(
            fixture.context_for(update_operation),
            first.entity_id,
            first.revision,
            cortex_application::MemoryCorrectInput {
                statement: "First corrected".to_owned(),
                normalized_subject: "subject".to_owned(),
                normalized_predicate: "predicate".to_owned(),
                normalized_object: "object".to_owned(),
                sources: vec![cortex_domain::SourceRef { source_id }],
            },
        )
        .await
        .map_err(debug_error)?;

    // The same operation ID against a different target must conflict, not
    // return the recorded result or mutate the second memory.
    let conflicted = fixture
        .service
        .correct_memory(
            fixture.context_for(update_operation),
            second.entity_id,
            second.revision,
            cortex_application::MemoryCorrectInput {
                statement: "Second corrected".to_owned(),
                normalized_subject: "subject".to_owned(),
                normalized_predicate: "predicate".to_owned(),
                normalized_object: "object".to_owned(),
                sources: vec![cortex_domain::SourceRef { source_id }],
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
async fn memory_audit_targets_are_recorded_without_content() -> Result<(), String> {
    let fixture = Fixture::all_mutations();
    let source_id = fixture.seed_source("seed")?;
    let created = fixture
        .service
        .create_memory(
            fixture.context(),
            MemoryCreateInput {
                statement: "Secret statement".to_owned(),
                normalized_subject: "subject".to_owned(),
                normalized_predicate: "predicate".to_owned(),
                normalized_object: "object".to_owned(),
                sources: vec![cortex_domain::SourceRef { source_id }],
            },
        )
        .await
        .map_err(debug_error)?;
    let deleted = fixture
        .service
        .delete_memory(fixture.context(), created.entity_id, created.revision)
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
    assert!(!rendered.contains("Secret statement"));
    Ok(())
}
