//! Provider-backed application authority for user-authored knowledge and
//! tasks (SCRUM-116; storage plan §13 steps 5-7).
//!
//! This is the production command/query layer for user content: every
//! operation evaluates Cortex policy before any provider dispatch, preserves
//! operation identity with idempotent replay, forwards the caller's expected
//! revision untouched to the provider, and returns typed redacted results.
//! Nothing in this module reads or mutates canonical `SQLite` note/task rows —
//! those repositories are migration state only.

use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, ObservedRevision, ProviderAuditMetadata,
    ProviderResourceKind, ProviderResourceRef, ResourceTarget,
};

use crate::{
    ApplicationError, AuditPort, Capability, CommandContext, KnowledgeCreate, KnowledgeDelete,
    KnowledgeDocument, KnowledgeProvider, KnowledgeQuery, KnowledgeUpdate, OperationIdentity,
    PolicyDecision, PolicyPort, ProviderError, ProviderMutation, ProviderPage, ProviderRead,
    ProviderTask, TaskComplete, TaskCreate, TaskDelete, TaskProvider, TaskQuery, TaskUpdate,
};

/// Typed result of one provider-backed mutation: the addressed resource plus
/// the before/after observed revisions retained for audit and replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderMutationOutcome {
    pub resource: ProviderResourceRef,
    pub previous_revision: Option<ObservedRevision>,
    pub current_revision: Option<ObservedRevision>,
}

impl ProviderMutationOutcome {
    fn from_mutation(mutation: &ProviderMutation) -> Self {
        Self {
            resource: mutation.resource().clone(),
            previous_revision: mutation
                .previous()
                .map(|provenance| provenance.observed_revision().clone()),
            current_revision: mutation
                .current()
                .map(|provenance| provenance.observed_revision().clone()),
        }
    }
}

/// Outcome of the preflight: either an idempotent replay of an identical
/// recorded operation, or the identity to settle a fresh dispatch under.
enum Begin {
    Replay(ProviderMutationOutcome),
    Proceed(OperationIdentity),
}

/// A recorded provider mutation retained for idempotent replay: the exact
/// command identity that produced it plus its typed outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderOperationRecord {
    pub identity: OperationIdentity,
    pub outcome: ProviderMutationOutcome,
}

/// Durable idempotency log for provider-backed mutations. Implementations
/// must scope records by workspace and operation, and return an existing
/// record instead of overwriting it when the same operation is recorded twice.
#[allow(async_fn_in_trait)]
pub trait ProviderOperationLog: Send + Sync {
    async fn find(
        &self,
        workspace_id: cortex_domain::WorkspaceId,
        operation_id: cortex_domain::OperationId,
    ) -> Result<Option<ProviderOperationRecord>, ApplicationError>;

    async fn record(
        &self,
        workspace_id: cortex_domain::WorkspaceId,
        operation_id: cortex_domain::OperationId,
        record: ProviderOperationRecord,
    ) -> Result<(), ApplicationError>;
}

/// The policy boundary required by the provider authority.
pub trait AuthorityPolicy: Send + Sync {
    fn evaluate(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<&ResourceTarget>,
    ) -> PolicyDecision;
}

impl<T: PolicyPort> AuthorityPolicy for T {
    fn evaluate(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<&ResourceTarget>,
    ) -> PolicyDecision {
        PolicyPort::evaluate(self, context, capability, target)
    }
}

/// Policy-gated, idempotent provider authority over one knowledge and one
/// task provider.
pub struct ProviderAuthority<P, K, T, L, A> {
    policy: P,
    knowledge: K,
    tasks: T,
    operations: L,
    audit: A,
}

impl<P, K, T, L, A> ProviderAuthority<P, K, T, L, A> {
    /// Binds the policy boundary, both providers, the operation log, and the
    /// audit port.
    #[must_use]
    pub const fn new(policy: P, knowledge: K, tasks: T, operations: L, audit: A) -> Self {
        Self {
            policy,
            knowledge,
            tasks,
            operations,
            audit,
        }
    }
}

impl<P, K, T, L, A> ProviderAuthority<P, K, T, L, A>
where
    P: AuthorityPolicy,
    K: KnowledgeProvider,
    T: TaskProvider,
    L: ProviderOperationLog,
    A: AuditPort,
{
    /// Creates a document through the knowledge provider.
    ///
    /// # Errors
    /// Returns typed policy, conflict, validation, or storage errors.
    pub async fn create_knowledge(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: KnowledgeCreate,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_create(context, capability);
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.knowledge.create(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Updates a document through the knowledge provider, forwarding the
    /// caller's expected revision untouched.
    ///
    /// # Errors
    /// Returns typed policy, not-found, conflict, validation, or storage errors.
    pub async fn update_knowledge(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: KnowledgeUpdate,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_resource(context, capability, input.resource());
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.knowledge.update(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Deletes a document through the knowledge provider.
    ///
    /// # Errors
    /// Returns typed policy, not-found, conflict, validation, or storage errors.
    pub async fn delete_knowledge(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: KnowledgeDelete,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_resource(context, capability, input.resource());
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.knowledge.delete(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Creates a task through the task provider.
    ///
    /// # Errors
    /// Returns typed policy, conflict, validation, or storage errors.
    pub async fn create_task(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: TaskCreate,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_create(context, capability);
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.tasks.create(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Updates a task through the task provider, forwarding the caller's
    /// expected revision untouched.
    ///
    /// # Errors
    /// Returns typed policy, not-found, conflict, validation, or storage errors.
    pub async fn update_task(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: TaskUpdate,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_resource(context, capability, input.resource());
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.tasks.update(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Completes a task through the task provider.
    ///
    /// # Errors
    /// Returns typed policy, not-found, conflict, validation, or storage errors.
    pub async fn complete_task(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: TaskComplete,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_resource(context, capability, input.resource());
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.tasks.complete(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Deletes a task through the task provider.
    ///
    /// # Errors
    /// Returns typed policy, not-found, conflict, validation, or storage errors.
    pub async fn delete_task(
        &self,
        context: &CommandContext,
        capability: Capability,
        input: TaskDelete,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let scope = AuthorityScope::for_resource(context, capability, input.resource());
        match self.begin(&scope).await? {
            Begin::Replay(outcome) => Ok(outcome),
            Begin::Proceed(identity) => {
                let mutation = self.tasks.delete(input).await;
                self.settle(&scope, identity, mutation).await
            }
        }
    }

    /// Loads one task through the task provider after policy evaluation.
    ///
    /// # Errors
    /// Returns typed policy, validation, or storage errors.
    pub async fn find_task(
        &self,
        context: &CommandContext,
        resource: ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ApplicationError> {
        self.require(
            context,
            Capability::TaskList,
            ResourceTarget::ProviderResource(resource.clone()),
        )
        .await?;
        TaskProvider::get(&self.tasks, &resource)
            .await
            .map_err(|error| map_provider_error(Capability::TaskList, &error))
    }

    /// Lists tasks through the task provider after policy evaluation.
    ///
    /// # Errors
    /// Returns typed policy, validation, or storage errors.
    pub async fn list_tasks(
        &self,
        context: &CommandContext,
        query: TaskQuery,
    ) -> Result<ProviderPage<ProviderTask>, ApplicationError> {
        self.require(
            context,
            Capability::TaskList,
            ResourceTarget::ProviderScope {
                provider_id: cortex_domain::ProviderId::new("authority")
                    .map_err(|_| ApplicationError::Internal)?,
                workspace_id: query.workspace_id(),
                resource_kind: ProviderResourceKind::Task,
            },
        )
        .await?;
        TaskProvider::search(&self.tasks, &query)
            .await
            .map_err(|error| map_provider_error(Capability::TaskList, &error))
    }

    /// Loads one document through the knowledge provider after policy
    /// evaluation.
    ///
    /// # Errors
    /// Returns typed policy, validation, or storage errors.
    pub async fn find_document(
        &self,
        context: &CommandContext,
        resource: ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ApplicationError> {
        self.require(
            context,
            Capability::KnowledgeRetrieve,
            ResourceTarget::ProviderResource(resource.clone()),
        )
        .await?;
        KnowledgeProvider::get(&self.knowledge, &resource)
            .await
            .map_err(|error| map_provider_error(Capability::KnowledgeRetrieve, &error))
    }

    /// Searches documents through the knowledge provider after policy
    /// evaluation.
    ///
    /// # Errors
    /// Returns typed policy, validation, or storage errors.
    pub async fn search_documents(
        &self,
        context: &CommandContext,
        query: KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ApplicationError> {
        self.require(
            context,
            Capability::KnowledgeRetrieve,
            ResourceTarget::ProviderScope {
                provider_id: cortex_domain::ProviderId::new("authority")
                    .map_err(|_| ApplicationError::Internal)?,
                workspace_id: query.workspace_id(),
                resource_kind: ProviderResourceKind::Knowledge,
            },
        )
        .await?;
        KnowledgeProvider::search(&self.knowledge, &query)
            .await
            .map_err(|error| map_provider_error(Capability::KnowledgeRetrieve, &error))
    }

    /// Read-only policy requirement: a deny is a typed policy failure
    /// without audit (reads carry no operation identity).
    #[allow(clippy::unused_async)] // kept async for uniform call sites
    async fn require(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: ResourceTarget,
    ) -> Result<(), ApplicationError> {
        if let PolicyDecision::Deny(deny) = self.policy.evaluate(context, capability, Some(&target))
        {
            return Err(ApplicationError::PolicyDenied(deny));
        }
        Ok(())
    }

    /// Evaluates policy, resolves idempotent replay, and yields either the
    /// recorded outcome of an identical earlier operation or the operation
    /// identity the provider dispatch is settled under.
    async fn begin(&self, scope: &AuthorityScope<'_>) -> Result<Begin, ApplicationError> {
        if let PolicyDecision::Deny(deny) =
            self.policy
                .evaluate(scope.context, scope.capability, scope.target.as_ref())
        {
            let _ = self
                .audit
                .append(audit_event(
                    scope.context,
                    scope.capability,
                    scope.target.clone(),
                    PolicyDecision::Deny(deny),
                    AuditResult::Rejected,
                ))
                .await;
            return Err(ApplicationError::PolicyDenied(deny));
        }
        let identity = OperationIdentity::new(
            scope.context.principal_id,
            scope.capability,
            scope.target.clone(),
        );
        if let Some(recorded) = self
            .operations
            .find(scope.context.workspace_id, scope.context.operation_id)
            .await?
        {
            if recorded.identity == identity {
                return Ok(Begin::Replay(recorded.outcome));
            }
            return Err(ApplicationError::Conflict {
                entity: "operation",
            });
        }
        Ok(Begin::Proceed(identity))
    }

    /// Maps the provider result, records the operation, and appends success
    /// or failure audit evidence.
    async fn settle(
        &self,
        scope: &AuthorityScope<'_>,
        identity: OperationIdentity,
        mutation: Result<ProviderMutation, ProviderError>,
    ) -> Result<ProviderMutationOutcome, ApplicationError> {
        let mutation = match mutation {
            Ok(mutation) => mutation,
            Err(error) => {
                let mapped = map_provider_error(scope.capability, &error);
                let result = match &mapped {
                    ApplicationError::Storage(_) | ApplicationError::Internal => {
                        AuditResult::Failed
                    }
                    _ => AuditResult::Rejected,
                };
                let _ = self
                    .audit
                    .append(audit_event(
                        scope.context,
                        scope.capability,
                        scope.target.clone(),
                        PolicyDecision::Allow,
                        result,
                    ))
                    .await;
                return Err(mapped);
            }
        };

        let outcome = ProviderMutationOutcome::from_mutation(&mutation);
        self.operations
            .record(
                scope.context.workspace_id,
                scope.context.operation_id,
                ProviderOperationRecord {
                    identity,
                    outcome: outcome.clone(),
                },
            )
            .await?;
        self.audit
            .append(AuditEvent {
                id: AuditEventId::new(),
                workspace_id: scope.context.workspace_id,
                principal_id: scope.context.principal_id,
                operation_id: scope.context.operation_id,
                correlation_id: scope.context.correlation_id,
                capability: scope.capability.metadata().mcp_name,
                target: Some(ResourceTarget::ProviderResource(outcome.resource.clone())),
                provider_metadata: Some(ProviderAuditMetadata::new(
                    outcome.previous_revision.clone(),
                    None,
                    outcome.current_revision.clone(),
                    None,
                )),
                policy_decision: PolicyDecision::Allow,
                result: AuditResult::Succeeded,
            })
            .await?;
        Ok(outcome)
    }
}

/// The policy/identity scope of one mutation, derived from its input.
struct AuthorityScope<'a> {
    context: &'a CommandContext,
    capability: Capability,
    target: Option<ResourceTarget>,
}

impl<'a> AuthorityScope<'a> {
    fn for_create(context: &'a CommandContext, capability: Capability) -> Self {
        Self {
            context,
            capability,
            target: None,
        }
    }

    fn for_resource(
        context: &'a CommandContext,
        capability: Capability,
        resource: &ProviderResourceRef,
    ) -> Self {
        Self {
            context,
            capability,
            target: Some(ResourceTarget::ProviderResource(resource.clone())),
        }
    }
}

fn map_provider_error(capability: Capability, error: &ProviderError) -> ApplicationError {
    let entity = match capability {
        Capability::TaskCreate
        | Capability::TaskUpdate
        | Capability::TaskComplete
        | Capability::TaskDelete => "task",
        _ => "knowledge_document",
    };
    match error {
        ProviderError::Validation { field } => ApplicationError::Validation { field },
        ProviderError::Unauthorized => ApplicationError::PermissionDenied,
        ProviderError::NotFound { .. } => ApplicationError::NotFound { entity },
        ProviderError::Conflict { .. } => ApplicationError::Conflict { entity },
        ProviderError::Unavailable => ApplicationError::Storage("provider unavailable".to_owned()),
        ProviderError::Internal => ApplicationError::Internal,
    }
}

fn audit_event(
    context: &CommandContext,
    capability: Capability,
    target: Option<ResourceTarget>,
    decision: PolicyDecision,
    result: AuditResult,
) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        workspace_id: context.workspace_id,
        principal_id: context.principal_id,
        operation_id: context.operation_id,
        correlation_id: context.correlation_id,
        capability: capability.metadata().mcp_name,
        target,
        provider_metadata: None,
        policy_decision: decision,
        result,
    }
}
