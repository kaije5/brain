use std::collections::BTreeSet;

use cortex_domain::{
    AuditEvent, MemoryAssertionInput, PolicyDecision, PolicyDeny, PrincipalId, WorkspaceId,
};

use crate::{ApplicationError, Capability, CommandContext, MutationResult};

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
    fn evaluate(&self, context: &CommandContext, capability: Capability) -> PolicyDecision;
}

/// The append-only redacted audit boundary used by application commands.
#[allow(async_fn_in_trait)]
pub trait AuditPort: Send + Sync {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError>;
}

/// The typed capability boundary made available to a local agent.
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
        input: MemoryAssertionInput,
    ) -> Result<MutationResult, ApplicationError>;
}

/// A deterministic deny-by-default policy backed by explicit capability grants.
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
    fn evaluate(&self, context: &CommandContext, capability: Capability) -> PolicyDecision {
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
