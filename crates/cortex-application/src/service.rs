use std::collections::BTreeSet;

use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Lifecycle, MemoryAssertion,
    MemoryAssertionInput, MemoryStatus, PolicyDecision, PolicyDeny, PrincipalId, ResourceTarget,
    Revision, WorkspaceId,
};

use crate::{
    AggregateChange, ApplicationError, AtomicMutation, AtomicMutationPort, Capability,
    CommandContext, MemoryCorrectInput, MemoryCreateInput, MemoryRepository, MutationResult,
    OperationResultRepository, SourceRepository,
};

/// A workspace-scoped capability grant issued by Cortex-owned configuration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapabilityGrant {
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    capability: Capability,
}

impl CapabilityGrant {
    #[must_use]
    pub const fn new(
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        capability: Capability,
    ) -> Self {
        Self {
            workspace_id,
            principal_id,
            capability,
        }
    }
}

/// The policy boundary evaluated by Cortex before every capability invocation.
pub trait PolicyPort: Send + Sync {
    fn evaluate(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<&ResourceTarget>,
    ) -> PolicyDecision;
}

/// Append-only redacted audit boundary for rejected or failed commands.
/// Successful mutation evidence is committed through [`AtomicMutationPort`].
#[allow(async_fn_in_trait)]
pub trait AuditPort: Send + Sync {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError>;
}

/// Typed capability boundary made available to a local agent through static dispatch.
#[allow(async_fn_in_trait)]
pub trait AgentCapabilityExecutor: Send + Sync {
    async fn execute_agent_tool(
        &self,
        context: CommandContext,
        capability: Capability,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ApplicationError>;
}

/// Application use cases shared by every trusted Cortex transport adapter.
#[allow(async_fn_in_trait)]
pub trait CortexService: AgentCapabilityExecutor {
    async fn create_memory(
        &self,
        context: CommandContext,
        input: MemoryCreateInput,
    ) -> Result<MutationResult, ApplicationError>;
    async fn correct_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
        input: MemoryCorrectInput,
    ) -> Result<MutationResult, ApplicationError>;
    async fn delete_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;
    async fn restore_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError>;
}

/// Transactional implementation of memory commands.
pub struct ApplicationService<P, R, M, A> {
    policy: P,
    repositories: R,
    mutations: M,
    audit: A,
}

impl<P, R, M, A> ApplicationService<P, R, M, A> {
    #[must_use]
    pub const fn new(policy: P, repositories: R, mutations: M, audit: A) -> Self {
        Self {
            policy,
            repositories,
            mutations,
            audit,
        }
    }
}

impl<P, R, M, A> ApplicationService<P, R, M, A>
where
    P: PolicyPort,
    R: MemoryRepository + SourceRepository,
    M: AtomicMutationPort + OperationResultRepository,
    A: AuditPort,
{
    /// # Errors
    /// Returns a typed policy, validation, audit, or storage error.
    pub async fn create_memory(
        &self,
        context: CommandContext,
        input: MemoryCreateInput,
    ) -> Result<MutationResult, ApplicationError> {
        let capability = Capability::MemoryCreate;
        if let Some(result) = self.preflight(&context, capability, None).await? {
            return Ok(result);
        }
        self.validate_sources(&context, capability, None, &input.sources)
            .await?;
        let memory = self
            .audit_result(
                &context,
                capability,
                None,
                MemoryAssertion::create(MemoryAssertionInput {
                    workspace_id: context.workspace_id,
                    statement: input.statement,
                    normalized_subject: input.normalized_subject,
                    normalized_predicate: input.normalized_predicate,
                    normalized_object: input.normalized_object,
                    sources: input.sources,
                })
                .map_err(ApplicationError::from),
            )
            .await?;
        self.commit(
            context,
            capability,
            None,
            mutation_result(&context, memory.id(), memory.revision(), memory.lifecycle()),
            vec![AggregateChange::InsertMemory(memory)],
        )
        .await
    }

    /// # Errors
    /// Returns not-found, conflict, policy, validation, provenance, audit, or storage errors.
    pub async fn correct_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
        input: MemoryCorrectInput,
    ) -> Result<MutationResult, ApplicationError> {
        let capability = Capability::MemoryCorrect;
        if let Some(result) = self
            .preflight(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
            )
            .await?
        {
            return Ok(result);
        }
        let loaded =
            MemoryRepository::find_history(&self.repositories, context.workspace_id, entity_id)
                .await
                .and_then(|value| value.ok_or(ApplicationError::NotFound { entity: "memory" }));
        let predecessor = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                loaded,
            )
            .await?;
        self.audit_result(
            &context,
            capability,
            Some(ResourceTarget::CortexEntity(entity_id)),
            require_memory_active(&predecessor, expected_revision),
        )
        .await?;
        self.validate_sources(
            &context,
            capability,
            Some(ResourceTarget::CortexEntity(entity_id)),
            &input.sources,
        )
        .await?;
        let successor = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                predecessor
                    .correct(MemoryAssertionInput {
                        workspace_id: context.workspace_id,
                        statement: input.statement,
                        normalized_subject: input.normalized_subject,
                        normalized_predicate: input.normalized_predicate,
                        normalized_object: input.normalized_object,
                        sources: input.sources,
                    })
                    .map_err(ApplicationError::from),
            )
            .await?;
        let predecessor_revision = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                expected_revision.next().map_err(ApplicationError::from),
            )
            .await?;
        let superseded = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                memory_with_state(
                    &predecessor,
                    predecessor_revision,
                    Lifecycle::Active,
                    MemoryStatus::Superseded,
                ),
            )
            .await?;
        let result_id = successor.id();
        let result_revision = successor.revision();
        let result_lifecycle = successor.lifecycle();
        self.commit(
            context,
            capability,
            Some(ResourceTarget::CortexEntity(entity_id)),
            mutation_result(&context, result_id, result_revision, result_lifecycle),
            vec![
                AggregateChange::ReplaceMemory {
                    entity_id,
                    expected_revision,
                    memory: superseded,
                },
                AggregateChange::InsertMemory(successor),
            ],
        )
        .await
    }

    /// # Errors
    /// Returns not-found, conflict, policy, audit, or storage errors.
    pub async fn delete_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError> {
        self.change_memory_lifecycle(context, entity_id, expected_revision, Lifecycle::Deleted)
            .await
    }

    /// # Errors
    /// Returns not-found, conflict, policy, audit, or storage errors.
    pub async fn restore_memory(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
    ) -> Result<MutationResult, ApplicationError> {
        self.change_memory_lifecycle(context, entity_id, expected_revision, Lifecycle::Active)
            .await
    }

    async fn change_memory_lifecycle(
        &self,
        context: CommandContext,
        entity_id: EntityId,
        expected_revision: Revision,
        target: Lifecycle,
    ) -> Result<MutationResult, ApplicationError> {
        let (capability, required, change) = match target {
            Lifecycle::Deleted => (
                Capability::MemoryDelete,
                Lifecycle::Active,
                AggregateChange::DeleteMemory {
                    entity_id,
                    expected_revision,
                },
            ),
            Lifecycle::Active => (
                Capability::MemoryRestore,
                Lifecycle::Deleted,
                AggregateChange::RestoreMemory {
                    entity_id,
                    expected_revision,
                },
            ),
        };
        if let Some(result) = self
            .preflight(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
            )
            .await?
        {
            return Ok(result);
        }
        let loaded =
            MemoryRepository::find_history(&self.repositories, context.workspace_id, entity_id)
                .await
                .and_then(|value| value.ok_or(ApplicationError::NotFound { entity: "memory" }));
        let memory = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                loaded,
            )
            .await?;
        self.audit_result(
            &context,
            capability,
            Some(ResourceTarget::CortexEntity(entity_id)),
            require_state(
                memory.revision(),
                expected_revision,
                memory.lifecycle(),
                required,
                "memory",
            ),
        )
        .await?;
        let revision = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(entity_id)),
                expected_revision.next().map_err(ApplicationError::from),
            )
            .await?;
        self.commit(
            context,
            capability,
            Some(ResourceTarget::CortexEntity(entity_id)),
            mutation_result(&context, entity_id, revision, target),
            vec![change],
        )
        .await
    }

    async fn preflight(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<ResourceTarget>,
    ) -> Result<Option<MutationResult>, ApplicationError> {
        let decision = self.policy.evaluate(context, capability, target.as_ref());
        let previous = self
            .mutations
            .find_result(context.workspace_id, context.operation_id)
            .await?;
        match decision {
            PolicyDecision::Allow => match previous {
                Some(recorded)
                    if recorded.identity
                        == crate::OperationIdentity::new(
                            context.principal_id,
                            capability,
                            target.clone(),
                        ) =>
                {
                    Ok(Some(recorded.result))
                }
                Some(_) => Err(ApplicationError::Conflict {
                    entity: "operation",
                }),
                None => Ok(None),
            },
            PolicyDecision::Deny(deny) => {
                let error = ApplicationError::PolicyDenied(deny);
                // A completed operation already has its one canonical atomic audit row.
                // Re-evaluate policy on replay, but do not append a conflicting second row.
                if previous.is_some() {
                    return Err(error);
                }
                Err(self
                    .record_error(
                        context,
                        capability,
                        target.clone(),
                        PolicyDecision::Deny(deny),
                        error,
                    )
                    .await)
            }
        }
    }

    async fn validate_sources(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<ResourceTarget>,
        sources: &[cortex_domain::SourceRef],
    ) -> Result<(), ApplicationError> {
        for source in sources {
            let found =
                SourceRepository::find(&self.repositories, context.workspace_id, source.source_id)
                    .await;
            let found = self
                .audit_result(context, capability, target.clone(), found)
                .await?;
            if !matches!(found, Some(source) if source.lifecycle() == Lifecycle::Active) {
                return Err(self
                    .record_error(
                        context,
                        capability,
                        target.clone(),
                        PolicyDecision::Allow,
                        ApplicationError::NotFound { entity: "source" },
                    )
                    .await);
            }
        }
        Ok(())
    }

    async fn commit(
        &self,
        context: CommandContext,
        capability: Capability,
        target: Option<ResourceTarget>,
        result: MutationResult,
        changes: Vec<AggregateChange>,
    ) -> Result<MutationResult, ApplicationError> {
        let mutation = self
            .audit_result(
                &context,
                capability,
                Some(ResourceTarget::CortexEntity(result.entity_id)),
                AtomicMutation::new(
                    context,
                    capability,
                    target,
                    changes,
                    result,
                    audit_event(
                        &context,
                        capability,
                        Some(ResourceTarget::CortexEntity(result.entity_id)),
                        PolicyDecision::Allow,
                        AuditResult::Succeeded,
                    ),
                ),
            )
            .await?;
        let executed = self.mutations.execute_once(mutation).await;
        self.audit_result(
            &context,
            capability,
            Some(ResourceTarget::CortexEntity(result.entity_id)),
            executed,
        )
        .await
    }

    async fn audit_result<T>(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<ResourceTarget>,
        result: Result<T, ApplicationError>,
    ) -> Result<T, ApplicationError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => Err(self
                .record_error(context, capability, target, PolicyDecision::Allow, error)
                .await),
        }
    }

    async fn record_error(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<ResourceTarget>,
        decision: PolicyDecision,
        error: ApplicationError,
    ) -> ApplicationError {
        let result = match error {
            ApplicationError::Storage(_) | ApplicationError::Internal => AuditResult::Failed,
            _ => AuditResult::Rejected,
        };
        match self
            .audit
            .append(audit_event(context, capability, target, decision, result))
            .await
        {
            Ok(()) => error,
            Err(audit_error) => audit_error,
        }
    }
}

/// Deterministic deny-by-default policy backed by explicit capability grants.
pub struct GrantPolicy {
    grants: BTreeSet<CapabilityGrant>,
}

impl GrantPolicy {
    #[must_use]
    pub fn new(grants: impl IntoIterator<Item = CapabilityGrant>) -> Self {
        Self {
            grants: grants.into_iter().collect(),
        }
    }
}

impl PolicyPort for GrantPolicy {
    fn evaluate(
        &self,
        context: &CommandContext,
        capability: Capability,
        target: Option<&ResourceTarget>,
    ) -> PolicyDecision {
        // A provider target may only be addressed inside the authenticated
        // workspace, regardless of which capability grants the principal holds.
        let target_in_workspace = match target {
            None | Some(ResourceTarget::CortexEntity(_)) => true,
            Some(ResourceTarget::ProviderResource(resource)) => {
                resource.workspace_id() == context.workspace_id
            }
            Some(ResourceTarget::ProviderScope { workspace_id, .. }) => {
                *workspace_id == context.workspace_id
            }
        };
        if !target_in_workspace {
            return PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace);
        }
        let grant = CapabilityGrant::new(
            context.workspace_id,
            context.principal_id,
            capability.metadata().required_grant,
        );
        if self.grants.contains(&grant) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(PolicyDeny::MissingGrant)
        }
    }
}

fn require_state(
    actual_revision: Revision,
    expected_revision: Revision,
    actual_lifecycle: Lifecycle,
    required_lifecycle: Lifecycle,
    entity: &'static str,
) -> Result<(), ApplicationError> {
    if actual_revision != expected_revision || actual_lifecycle != required_lifecycle {
        return Err(ApplicationError::Conflict { entity });
    }
    Ok(())
}

fn require_memory_active(
    memory: &MemoryAssertion,
    expected_revision: Revision,
) -> Result<(), ApplicationError> {
    require_state(
        memory.revision(),
        expected_revision,
        memory.lifecycle(),
        Lifecycle::Active,
        "memory",
    )?;
    if memory.status() != MemoryStatus::Active {
        return Err(ApplicationError::Conflict { entity: "memory" });
    }
    Ok(())
}

fn memory_with_state(
    memory: &MemoryAssertion,
    revision: Revision,
    lifecycle: Lifecycle,
    status: MemoryStatus,
) -> Result<MemoryAssertion, ApplicationError> {
    MemoryAssertion::rehydrate(
        memory.id(),
        memory.workspace_id(),
        memory.statement().to_owned(),
        memory.normalized_subject().to_owned(),
        memory.normalized_predicate().to_owned(),
        memory.normalized_object().to_owned(),
        memory.sources().to_vec(),
        memory.supersedes(),
        status,
        revision,
        lifecycle,
    )
    .map_err(ApplicationError::from)
}

fn audit_event(
    context: &CommandContext,
    capability: Capability,
    target: Option<ResourceTarget>,
    policy_decision: PolicyDecision,
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
        policy_decision,
        result,
    }
}

fn mutation_result(
    context: &CommandContext,
    entity_id: EntityId,
    revision: Revision,
    lifecycle: Lifecycle,
) -> MutationResult {
    MutationResult {
        entity_id,
        revision,
        lifecycle,
        audit_correlation_id: context.correlation_id,
    }
}
