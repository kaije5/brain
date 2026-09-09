use cortex_application::{
    ApplicationError, Capability, CommandContext, GrantPolicy, PolicyDecision, PolicyDeny,
    PolicyPort,
};
use cortex_domain::{DomainError, OperationId, PrincipalId, WorkspaceId};
use uuid::Uuid;

#[test]
fn delete_requires_an_explicit_matching_grant() {
    let policy = GrantPolicy::new([]);
    let context = CommandContext::from_authenticated(
        WorkspaceId::new(),
        PrincipalId::new(),
        OperationId::new(),
        Uuid::now_v7(),
    );

    assert_eq!(
        policy.evaluate(&context, Capability::MemoryDelete),
        PolicyDecision::Deny(PolicyDeny::MissingGrant)
    );
}

#[test]
fn grant_for_another_capability_does_not_authorize_delete() {
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let policy = GrantPolicy::new([cortex_application::CapabilityGrant::new(
        workspace_id,
        principal_id,
        Capability::MemoryCreate,
    )]);
    let context = CommandContext::from_authenticated(
        workspace_id,
        principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    );

    assert_eq!(
        policy.evaluate(&context, Capability::MemoryDelete),
        PolicyDecision::Deny(PolicyDeny::MissingGrant)
    );
}

#[test]
fn domain_validation_maps_to_a_non_sensitive_application_error() {
    let error = ApplicationError::from(DomainError::validation(
        "title",
        "private source text must not be exposed",
    ));

    assert_eq!(error, ApplicationError::Validation { field: "title" });
}
