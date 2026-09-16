#![allow(clippy::missing_errors_doc, clippy::result_large_err)]

//! Idempotent automation triggered by normalized vault change events
//! (SCRUM-132).
//!
//! Triggers are normalized and transport-agnostic: automation never couples
//! to a filesystem watcher, sync product, or other event source — it sees
//! [`VaultChangeTrigger`]s only. Every (rule, event) pair runs at most once;
//! runs are recorded in Cortex-owned state through [`AutomationRunLog`], and
//! only after the handler succeeds — a failed handler is not recorded and
//! will run again when the trigger is redelivered.

use chrono::{DateTime, Utc};
use cortex_domain::{OperationId, WorkspaceId};

use crate::ApplicationError;

/// The normalized kind of a vault change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultChangeKind {
    Created,
    Updated,
    Deleted,
    Renamed,
}

/// A normalized, transport-agnostic vault change trigger. Identity is the
/// provider resource — never a filesystem path — and `event_id` is the
/// deduplication key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultChangeTrigger {
    event_id: OperationId,
    kind: VaultChangeKind,
    provider_id: String,
    resource_id: String,
}

impl VaultChangeTrigger {
    pub fn new(
        event_id: OperationId,
        kind: VaultChangeKind,
        provider_id: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Result<Self, ApplicationError> {
        let provider_id = provider_id.into();
        let resource_id = resource_id.into();
        if provider_id.trim().is_empty()
            || provider_id.len() > 128
            || resource_id.trim().is_empty()
            || resource_id.len() > 512
        {
            return Err(ApplicationError::Validation {
                field: "vault_change_trigger",
            });
        }
        Ok(Self {
            event_id,
            kind,
            provider_id,
            resource_id,
        })
    }

    #[must_use]
    pub const fn event_id(&self) -> OperationId {
        self.event_id
    }

    #[must_use]
    pub const fn kind(&self) -> VaultChangeKind {
        self.kind
    }

    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    #[must_use]
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }
}

/// Cortex-owned record of one executed automation run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationRunRecord {
    workspace_id: WorkspaceId,
    rule_id: String,
    event_id: OperationId,
    recorded_at: DateTime<Utc>,
}

impl AutomationRunRecord {
    #[must_use]
    pub fn new(
        workspace_id: WorkspaceId,
        rule_id: impl Into<String>,
        event_id: OperationId,
        recorded_at: DateTime<Utc>,
    ) -> Self {
        Self {
            workspace_id,
            rule_id: rule_id.into(),
            event_id,
            recorded_at,
        }
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub fn rule_id(&self) -> &str {
        &self.rule_id
    }

    #[must_use]
    pub const fn event_id(&self) -> OperationId {
        self.event_id
    }

    #[must_use]
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
}

/// Cortex-owned storage for automation run deduplication.
#[allow(async_fn_in_trait)]
pub trait AutomationRunLog: Send + Sync {
    /// Whether this (rule, event) pair already ran.
    async fn has_run(
        &self,
        workspace_id: WorkspaceId,
        rule_id: &str,
        event_id: OperationId,
    ) -> Result<bool, ApplicationError>;
    /// Records a completed run.
    async fn record(&self, run: AutomationRunRecord) -> Result<(), ApplicationError>;
}

/// The work a rule performs for one trigger. Handlers must be idempotent in
/// effect; delivery deduplication is the engine's job.
#[allow(async_fn_in_trait)]
pub trait VaultChangeHandler: Send + Sync {
    async fn handle(
        &self,
        workspace_id: WorkspaceId,
        trigger: &VaultChangeTrigger,
    ) -> Result<(), ApplicationError>;
}

/// What the engine did with a trigger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomationOutcome {
    /// The handler ran and the run was recorded.
    Ran,
    /// The (rule, event) pair already ran; the handler was skipped.
    Deduplicated,
}

/// Deterministic, idempotent automation engine over normalized vault change
/// events. Runs are Cortex-owned state; handler failures advance nothing.
pub struct AutomationEngine<H, L> {
    handler: H,
    log: L,
}

impl<H, L> AutomationEngine<H, L>
where
    H: VaultChangeHandler,
    L: AutomationRunLog,
{
    #[must_use]
    pub const fn new(handler: H, log: L) -> Self {
        Self { handler, log }
    }

    /// Delivers one trigger to one rule. Redelivery of an already-run
    /// (rule, event) pair is deduplicated. Handler failures surface as
    /// typed errors and leave run state untouched, so the trigger can be
    /// redelivered.
    ///
    /// # Errors
    /// Storage failures and handler failures are typed
    /// [`ApplicationError`]s.
    pub async fn handle(
        &self,
        workspace_id: WorkspaceId,
        rule_id: &str,
        trigger: &VaultChangeTrigger,
    ) -> Result<AutomationOutcome, ApplicationError> {
        if self
            .log
            .has_run(workspace_id, rule_id, trigger.event_id())
            .await?
        {
            return Ok(AutomationOutcome::Deduplicated);
        }
        self.handler.handle(workspace_id, trigger).await?;
        self.log
            .record(AutomationRunRecord::new(
                workspace_id,
                rule_id,
                trigger.event_id(),
                Utc::now(),
            ))
            .await?;
        Ok(AutomationOutcome::Ran)
    }
}
