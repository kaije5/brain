//! Policy-gated, audited decoration of the Markdown vault provider
//! (SCRUM-109; SCRUM-90 design "Policy, audit, and operation identity").
//!
//! Every provider operation evaluates Cortex policy for the exact principal,
//! workspace, capability, and [`ResourceTarget`] *before* any provider
//! dispatch; a deny is audited as `Rejected` and surfaces as
//! [`ProviderError::Unauthorized`]. Allowed mutations are audited as
//! `Succeeded` with before/after revision metadata. Audit evidence is
//! redacted: identifiers, classifications, and revisions only.

use cortex_application::{
    AuditPort, Capability, KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider,
    KnowledgeQuery, KnowledgeUpdate, PolicyDecision, PolicyPort, ProviderError, ProviderMutation,
    ProviderPage, ProviderRead, ProviderTask, TaskComplete, TaskCreate, TaskDelete, TaskProvider,
    TaskQuery, TaskUpdate,
};
use cortex_domain::{
    AuditEvent, AuditResult, OperationId, ProviderProvenance, ProviderResourceKind,
    ProviderResourceRef, ResourceTarget, WorkspaceId,
};
use uuid::Uuid;

use crate::vault_provider::MarkdownVaultProvider;

/// Policy-gated, audited decoration of the vault provider.
pub struct GovernedVaultProvider<P: PolicyPort, A: AuditPort> {
    inner: MarkdownVaultProvider,
    principal_id: cortex_domain::PrincipalId,
    policy: P,
    audit: A,
}

impl<P: PolicyPort, A: AuditPort> GovernedVaultProvider<P, A> {
    /// Wraps an opened vault provider with policy evaluation and audit.
    #[must_use]
    pub fn new(
        inner: MarkdownVaultProvider,
        principal_id: cortex_domain::PrincipalId,
        policy: P,
        audit: A,
    ) -> Self {
        Self {
            inner,
            principal_id,
            policy,
            audit,
        }
    }

    fn workspace(&self) -> WorkspaceId {
        self.inner.workspace_id()
    }

    /// Evaluates policy and audits a rejection. Returns `true` when allowed.
    async fn authorize(
        &self,
        capability: Capability,
        target: Option<ResourceTarget>,
        operation_id: OperationId,
    ) -> Result<(), ProviderError> {
        let context = cortex_application::CommandContext::from_authenticated(
            self.workspace(),
            self.principal_id,
            operation_id,
            Uuid::now_v7(),
        );
        let decision = self.policy.evaluate(&context, capability, target.as_ref());
        match decision {
            PolicyDecision::Allow => Ok(()),
            PolicyDecision::Deny(deny) => {
                let _ = self
                    .audit
                    .append(AuditEvent {
                        id: cortex_domain::AuditEventId::new(),
                        workspace_id: self.workspace(),
                        principal_id: self.principal_id,
                        operation_id,
                        correlation_id: Uuid::now_v7(),
                        capability: capability.metadata().mcp_name,
                        target,
                        provider_metadata: None,
                        policy_decision: PolicyDecision::Deny(deny),
                        result: AuditResult::Rejected,
                    })
                    .await;
                Err(ProviderError::Unauthorized)
            }
        }
    }

    /// Audits a completed mutation with before/after revision metadata.
    async fn audit_mutation(
        &self,
        capability: Capability,
        target: ResourceTarget,
        operation_id: OperationId,
        before_revision: Option<&cortex_domain::ObservedRevision>,
        after_revision: Option<&cortex_domain::ObservedRevision>,
    ) {
        let metadata = cortex_domain::ProviderAuditMetadata::new(
            before_revision.cloned(),
            None,
            after_revision.cloned(),
            None,
        );
        let _ = self
            .audit
            .append(AuditEvent {
                id: cortex_domain::AuditEventId::new(),
                workspace_id: self.workspace(),
                principal_id: self.principal_id,
                operation_id,
                correlation_id: Uuid::now_v7(),
                capability: capability.metadata().mcp_name,
                target: Some(target),
                provider_metadata: Some(metadata),
                policy_decision: PolicyDecision::Allow,
                result: AuditResult::Succeeded,
            })
            .await;
    }

    fn task_target(resource: &ProviderResourceRef) -> ResourceTarget {
        ResourceTarget::ProviderResource(resource.clone())
    }

    fn knowledge_scope(workspace_id: WorkspaceId) -> ResourceTarget {
        // The concrete document path is derived on create; the policy target
        // for creates is the scope (provider + workspace + kind).
        ResourceTarget::ProviderResource(ProviderResourceRef::new(
            workspace_id,
            cortex_domain::ProviderId::new("knowledge-scope")
                .unwrap_or_else(|_| cortex_domain::ProviderId::new("scope").expect("static id")),
            cortex_domain::ProviderResourceId::new("create-scope").unwrap_or_else(|_| {
                cortex_domain::ProviderResourceId::new("scope").expect("static id")
            }),
            ProviderResourceKind::Knowledge,
        ))
    }
}

impl<P: PolicyPort, A: AuditPort> KnowledgeProvider for GovernedVaultProvider<P, A> {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
        self.authorize(
            Capability::KnowledgeRetrieve,
            Some(ResourceTarget::ProviderResource(resource.clone())),
            OperationId::new(),
        )
        .await?;
        KnowledgeProvider::get(&self.inner, resource).await
    }

    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
        self.authorize(
            Capability::KnowledgeRetrieve,
            Some(ResourceTarget::ProviderScope {
                provider_id: cortex_domain::ProviderId::new("markdown-vault")
                    .map_err(|_| ProviderError::Internal)?,
                workspace_id: self.workspace(),
                resource_kind: ProviderResourceKind::Knowledge,
            }),
            OperationId::new(),
        )
        .await?;
        KnowledgeProvider::search(&self.inner, query).await
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        let target = Self::knowledge_scope(input.workspace_id());
        let operation_id = input.operation_id();
        self.authorize(
            Capability::KnowledgeCreate,
            Some(target.clone()),
            operation_id,
        )
        .await?;
        let mutation = KnowledgeProvider::create(&self.inner, input).await?;
        self.audit_mutation(
            Capability::KnowledgeCreate,
            target,
            operation_id,
            None,
            mutation
                .current()
                .map(ProviderProvenance::observed_revision),
        )
        .await;
        Ok(mutation)
    }

    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        let target = ResourceTarget::ProviderResource(input.resource().clone());
        let operation_id = input.operation_id();
        self.authorize(
            Capability::KnowledgeUpdate,
            Some(target.clone()),
            operation_id,
        )
        .await?;
        let mutation = KnowledgeProvider::update(&self.inner, input).await?;
        self.audit_mutation(
            Capability::KnowledgeUpdate,
            target,
            operation_id,
            mutation
                .previous()
                .map(ProviderProvenance::observed_revision),
            mutation
                .current()
                .map(ProviderProvenance::observed_revision),
        )
        .await;
        Ok(mutation)
    }

    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        let target = ResourceTarget::ProviderResource(input.resource().clone());
        let operation_id = input.operation_id();
        self.authorize(
            Capability::KnowledgeDelete,
            Some(target.clone()),
            operation_id,
        )
        .await?;
        let mutation = KnowledgeProvider::delete(&self.inner, input).await?;
        self.audit_mutation(
            Capability::KnowledgeDelete,
            target,
            operation_id,
            mutation
                .previous()
                .map(ProviderProvenance::observed_revision),
            None,
        )
        .await;
        Ok(mutation)
    }
}

impl<P: PolicyPort, A: AuditPort> TaskProvider for GovernedVaultProvider<P, A> {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        self.authorize(
            Capability::KnowledgeRetrieve,
            Some(Self::task_target(resource)),
            OperationId::new(),
        )
        .await?;
        TaskProvider::get(&self.inner, resource).await
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        self.authorize(
            Capability::KnowledgeRetrieve,
            Some(ResourceTarget::ProviderScope {
                provider_id: cortex_domain::ProviderId::new("markdown-vault")
                    .map_err(|_| ProviderError::Internal)?,
                workspace_id: self.workspace(),
                resource_kind: ProviderResourceKind::Task,
            }),
            OperationId::new(),
        )
        .await?;
        TaskProvider::search(&self.inner, query).await
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        let target = ResourceTarget::ProviderScope {
            provider_id: cortex_domain::ProviderId::new("markdown-vault")
                .map_err(|_| ProviderError::Internal)?,
            workspace_id: input.workspace_id(),
            resource_kind: ProviderResourceKind::Task,
        };
        let operation_id = input.operation_id();
        self.authorize(Capability::TaskCreate, Some(target.clone()), operation_id)
            .await?;
        let mutation = TaskProvider::create(&self.inner, input).await?;
        self.audit_mutation(
            Capability::TaskCreate,
            target,
            operation_id,
            None,
            mutation
                .current()
                .map(ProviderProvenance::observed_revision),
        )
        .await;
        Ok(mutation)
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        let target = Self::task_target(input.resource());
        let operation_id = input.operation_id();
        self.authorize(Capability::TaskUpdate, Some(target.clone()), operation_id)
            .await?;
        let mutation = TaskProvider::update(&self.inner, input).await?;
        self.audit_mutation(
            Capability::TaskUpdate,
            target,
            operation_id,
            mutation
                .previous()
                .map(ProviderProvenance::observed_revision),
            mutation
                .current()
                .map(ProviderProvenance::observed_revision),
        )
        .await;
        Ok(mutation)
    }

    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        let target = Self::task_target(input.resource());
        let operation_id = input.operation_id();
        self.authorize(Capability::TaskComplete, Some(target.clone()), operation_id)
            .await?;
        let mutation = TaskProvider::complete(&self.inner, input).await?;
        self.audit_mutation(
            Capability::TaskComplete,
            target,
            operation_id,
            mutation
                .previous()
                .map(ProviderProvenance::observed_revision),
            mutation
                .current()
                .map(ProviderProvenance::observed_revision),
        )
        .await;
        Ok(mutation)
    }

    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        let target = Self::task_target(input.resource());
        let operation_id = input.operation_id();
        self.authorize(Capability::TaskDelete, Some(target.clone()), operation_id)
            .await?;
        let mutation = TaskProvider::delete(&self.inner, input).await?;
        self.audit_mutation(
            Capability::TaskDelete,
            target,
            operation_id,
            mutation
                .previous()
                .map(ProviderProvenance::observed_revision),
            None,
        )
        .await;
        Ok(mutation)
    }
}
