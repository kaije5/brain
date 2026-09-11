use cortex_application::{
    AggregateChange, ApplicationError, AtomicMutation, Capability, CommandContext, MutationResult,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Note, NoteInput, OperationId, PolicyDecision,
    PrincipalId, ResourceTarget, WorkspaceId,
};
use uuid::Uuid;

#[test]
fn atomic_mutation_binds_changes_result_and_matching_audit_evidence_to_one_operation()
-> Result<(), String> {
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let operation_id = OperationId::new();
    let correlation_id = Uuid::now_v7();
    let note = note(workspace_id)?;
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: correlation_id,
    };
    let audit_event = audit_event(
        workspace_id,
        principal_id,
        operation_id,
        correlation_id,
        note.id(),
    );

    let mutation = AtomicMutation::new(
        CommandContext::from_authenticated(
            workspace_id,
            principal_id,
            operation_id,
            correlation_id,
        ),
        Capability::NoteCreate,
        None,
        vec![AggregateChange::InsertNote(note)],
        result,
        audit_event,
    );

    assert!(matches!(mutation, Ok(value) if value.result == result));
    Ok(())
}

#[test]
fn atomic_mutation_rejects_audit_evidence_from_another_operation() -> Result<(), String> {
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let operation_id = OperationId::new();
    let correlation_id = Uuid::now_v7();
    let note = note(workspace_id)?;
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: correlation_id,
    };
    let audit_event = audit_event(
        workspace_id,
        principal_id,
        OperationId::new(),
        correlation_id,
        note.id(),
    );

    let mutation = AtomicMutation::new(
        CommandContext::from_authenticated(
            workspace_id,
            principal_id,
            operation_id,
            correlation_id,
        ),
        Capability::NoteCreate,
        None,
        vec![AggregateChange::InsertNote(note)],
        result,
        audit_event,
    );

    assert_eq!(mutation, Err(ApplicationError::Internal));
    Ok(())
}

fn note(workspace_id: WorkspaceId) -> Result<Note, String> {
    Note::create(NoteInput {
        workspace_id,
        title: "atomic mutation".to_owned(),
        content: "audit evidence".to_owned(),
    })
    .map_err(|error| format!("valid note fixture rejected: {error:?}"))
}

fn audit_event(
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    operation_id: OperationId,
    correlation_id: Uuid,
    target_id: EntityId,
) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id,
        correlation_id,
        capability: "cortex_note_create",
        target: Some(ResourceTarget::CortexEntity(target_id)),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    }
}
