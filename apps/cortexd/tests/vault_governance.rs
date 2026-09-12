//! Policy-gated, audited vault mutations (SCRUM-109).

use std::collections::BTreeSet;
use std::sync::Mutex;

use cortex_application::{
    AuditPort, Capability, CapabilityGrant, GrantPolicy, KnowledgeCreate, KnowledgeDelete,
    KnowledgeProvider, PolicyDecision, PolicyDeny, PolicyPort, ProviderError, ProviderTaskPriority,
    TaskComplete, TaskCreate, TaskProvider, TaskSchedulingMetadata,
};
use cortex_domain::{AuditEvent, AuditResult, OperationId, ProviderResourceKind, WorkspaceId};
use cortexd::{
    GovernedVaultProvider, MarkdownVaultProvider, VaultProviderConfig, VaultProviderMode,
};
use tempfile::TempDir;
use uuid::Uuid;

#[derive(Default)]
struct RecordingAudit {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAudit {
    fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("audit lock").clone()
    }
}

impl AuditPort for &RecordingAudit {
    async fn append(&self, event: AuditEvent) -> Result<(), cortex_application::ApplicationError> {
        RecordingAudit::append(self, event).await
    }
}

impl AuditPort for RecordingAudit {
    async fn append(&self, event: AuditEvent) -> Result<(), cortex_application::ApplicationError> {
        eprintln!("AUDIT APPEND: {}", event.capability);
        self.events.lock().expect("audit lock").push(event);
        Ok(())
    }
}

fn scheduling() -> TaskSchedulingMetadata {
    TaskSchedulingMetadata::new(
        None,
        None,
        None,
        None,
        None,
        Option::<String>::None,
        Option::<String>::None,
    )
    .expect("empty scheduling is valid")
}

fn full_scopes() -> BTreeSet<cortexd::VaultScope> {
    let mut set = BTreeSet::new();
    set.insert(cortexd::VaultScope::new("knowledge").expect("valid"));
    set.insert(cortexd::VaultScope::new("task").expect("valid"));
    set
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::new()
}

fn governed<'a>(
    root: &'a std::path::Path,
    workspace_id: WorkspaceId,
    principal_id: cortex_domain::PrincipalId,
    grants: Vec<Capability>,
    audit: &'a RecordingAudit,
) -> GovernedVaultProvider<GrantPolicy, &'a RecordingAudit> {
    let config = VaultProviderConfig::new(
        "markdown-vault",
        root.to_path_buf(),
        VaultProviderMode::ReadWrite,
        full_scopes(),
        BTreeSet::new(),
    )
    .expect("valid config");
    let inner = MarkdownVaultProvider::open(config, workspace_id).expect("opens");
    let policy = GrantPolicy::new(
        grants
            .into_iter()
            .map(|capability| CapabilityGrant::new(workspace_id, principal_id, capability)),
    );
    GovernedVaultProvider::new(inner, principal_id, policy, audit)
}

#[tokio::test]
async fn allowed_create_dispatches_and_audits_success_with_revision_metadata() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = workspace_id();
    let principal_id = cortex_domain::PrincipalId::new();
    let audit = RecordingAudit::default();
    let provider = governed(
        directory.path(),
        workspace_id,
        principal_id,
        vec![Capability::KnowledgeCreate],
        &audit,
    );

    let mutation = KnowledgeProvider::create(
        &provider,
        KnowledgeCreate::new(workspace_id, OperationId::new(), "Audited", "audited body")
            .expect("valid create"),
    )
    .await
    .expect("granted create succeeds");

    let events = audit.events();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.capability, "cortex_knowledge_create");
    assert_eq!(event.principal_id, principal_id);
    assert_eq!(event.result, AuditResult::Succeeded);
    let metadata = event.provider_metadata.as_ref().expect("metadata recorded");
    let after = metadata.after_revision.as_ref().expect("after revision");
    assert_eq!(
        Some(after),
        mutation
            .current()
            .as_ref()
            .map(|provenance| provenance.observed_revision())
    );
}

#[tokio::test]
async fn denied_create_is_rejected_audited_and_never_dispatched() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = workspace_id();
    let principal_id = cortex_domain::PrincipalId::new();
    let audit = RecordingAudit::default();
    // No knowledge-create grant: policy denies before provider dispatch.
    let provider = governed(directory.path(), workspace_id, principal_id, vec![], &audit);

    let result = KnowledgeProvider::create(
        &provider,
        KnowledgeCreate::new(
            workspace_id,
            OperationId::new(),
            "Denied",
            "must not be written",
        )
        .expect("valid create"),
    )
    .await;
    assert!(matches!(result, Err(ProviderError::Unauthorized)));

    // The vault is untouched and the rejection is audited.
    assert!(!directory.path().join("Documents").exists());
    let events = audit.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].result, AuditResult::Rejected);
    assert_eq!(
        events[0].policy_decision,
        PolicyDecision::Deny(PolicyDeny::MissingGrant)
    );
    assert_eq!(events[0].capability, "cortex_knowledge_create");
}

#[tokio::test]
async fn delete_is_gated_by_the_destructive_capability_and_audited() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = workspace_id();
    let principal_id = cortex_domain::PrincipalId::new();
    let audit = RecordingAudit::default();
    let provider = governed(
        directory.path(),
        workspace_id,
        principal_id,
        vec![Capability::KnowledgeCreate, Capability::KnowledgeDelete],
        &audit,
    );

    let created = KnowledgeProvider::create(
        &provider,
        KnowledgeCreate::new(workspace_id, OperationId::new(), "Doomed", "doomed body")
            .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource().clone();
    let revision = created
        .current()
        .expect("created provenance")
        .observed_revision()
        .clone();

    KnowledgeProvider::delete(
        &provider,
        KnowledgeDelete::new(resource.clone(), OperationId::new(), revision).expect("valid delete"),
    )
    .await
    .expect("delete succeeds");

    let events = audit.events();
    let delete_event = events
        .iter()
        .find(|event| event.capability == "cortex_knowledge_delete")
        .expect("delete audited");
    assert_eq!(delete_event.result, AuditResult::Succeeded);
    // Destructive classification is retained on the capability.
    assert!(Capability::KnowledgeDelete.metadata().destructive);
    // The before revision is recorded; a deletion has no after revision.
    let metadata = delete_event.provider_metadata.as_ref().expect("metadata");
    assert!(metadata.before_revision.is_some());
    assert!(metadata.after_revision.is_none());
}

#[tokio::test]
async fn task_completion_requires_the_task_complete_grant() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = workspace_id();
    let principal_id = cortex_domain::PrincipalId::new();
    let audit = RecordingAudit::default();
    let provider = governed(
        directory.path(),
        workspace_id,
        principal_id,
        vec![Capability::TaskCreate],
        &audit,
    );

    let created = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            cortex_domain::TaskId::new(),
            "Gated",
            "body",
            ProviderTaskPriority::Normal,
            scheduling(),
        )
        .expect("valid create"),
    )
    .await
    .expect("granted create succeeds");
    let resource = created.resource().clone();
    let revision = created
        .current()
        .expect("provenance")
        .observed_revision()
        .clone();

    // The complete grant is missing: denied before dispatch.
    let denied = TaskProvider::complete(
        &provider,
        TaskComplete::new(resource.clone(), OperationId::new(), revision.clone())
            .expect("valid complete"),
    )
    .await;
    assert!(matches!(denied, Err(ProviderError::Unauthorized)));

    // Granting TaskComplete allows the mutation.
    let audit2 = RecordingAudit::default();
    let provider2 = governed(
        directory.path(),
        workspace_id,
        principal_id,
        vec![Capability::TaskCreate, Capability::TaskComplete],
        &audit2,
    );
    let completed = TaskProvider::complete(
        &provider2,
        TaskComplete::new(resource, OperationId::new(), revision).expect("valid complete"),
    )
    .await
    .expect("granted complete succeeds");
    assert!(
        completed
            .current()
            .expect("current")
            .observed_revision()
            .as_str()
            .starts_with("rev-")
    );
    let events = audit2.events();
    assert!(
        events
            .iter()
            .any(|event| event.capability == "cortex_task_complete"
                && event.result == AuditResult::Succeeded)
    );
}

#[test]
fn policy_evaluation_receives_the_provider_target() {
    // The governed wrapper always supplies a ResourceTarget; GrantPolicy
    // denies cross-workspace provider resources (SCRUM-99 integration).
    let workspace_id = WorkspaceId::new();
    let context = cortex_application::CommandContext::from_authenticated(
        workspace_id,
        cortex_domain::PrincipalId::new(),
        cortex_domain::OperationId::new(),
        Uuid::now_v7(),
    );
    let policy = GrantPolicy::new([]);
    let resource = cortex_domain::ProviderResourceRef::new(
        WorkspaceId::new(),
        cortex_domain::ProviderId::new("markdown-vault").expect("valid id"),
        cortex_domain::ProviderResourceId::new("r").expect("valid id"),
        ProviderResourceKind::Knowledge,
    );
    assert_eq!(
        GrantPolicy::evaluate(
            &policy,
            &context,
            Capability::KnowledgeRetrieve,
            Some(&cortex_domain::ResourceTarget::ProviderResource(resource)),
        ),
        PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace)
    );
}
