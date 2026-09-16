//! Review artifacts and accepted follow-up tasks through the vault
//! providers (SCRUM-131).
//!
//! Review artifacts are human-editable Markdown documents persisted through
//! [`KnowledgeProvider`] (vault authority); accepted follow-up commitments
//! become provider tasks through [`TaskProvider`]. Review execution state is
//! Cortex-owned and recorded through [`ReviewRunLog`] — but only after the
//! provider mutations succeed, so failed or conflicting mutations never
//! advance review state as if they succeeded.

use chrono::{DateTime, Utc};
use cortex_domain::{OperationId, TaskId, WorkspaceId};

use crate::{
    KnowledgeCreate, KnowledgeProvider, ProviderError, ProviderMutation, ProviderTaskPriority,
    TaskCreate, TaskProvider, TaskSchedulingMetadata,
};

/// Cortex-owned record of one persisted review artifact or accepted
/// follow-up, keyed by operation identity for idempotent replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRunRecord {
    workspace_id: WorkspaceId,
    operation_id: OperationId,
    kind: ReviewRunKind,
    recorded_at: DateTime<Utc>,
}

/// What kind of provider mutation the run committed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRunKind {
    ReviewArtifact,
    FollowUpTask,
}

impl ReviewRunRecord {
    #[must_use]
    pub const fn new(
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        kind: ReviewRunKind,
        recorded_at: DateTime<Utc>,
    ) -> Self {
        Self {
            workspace_id,
            operation_id,
            kind,
            recorded_at,
        }
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }

    #[must_use]
    pub const fn kind(&self) -> ReviewRunKind {
        self.kind
    }

    #[must_use]
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
}

/// Cortex-owned storage for review execution state.
#[allow(async_fn_in_trait)]
pub trait ReviewRunLog: Send + Sync {
    /// Records a completed run. Returns `false` when the operation identity
    /// was already recorded (idempotent replay).
    async fn record(&self, run: ReviewRunRecord) -> Result<bool, crate::ApplicationError>;
    /// Resolves the record for an operation identity, if any.
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<ReviewRunRecord>, crate::ApplicationError>;
}

/// Provider-backed review boundary: artifacts and follow-up commitments go
/// to the vault; execution state goes to Cortex, after success only.
pub struct ReviewService<K, T, L> {
    knowledge: K,
    tasks: T,
    log: L,
}

impl<K, T, L> ReviewService<K, T, L>
where
    K: KnowledgeProvider,
    T: TaskProvider,
    L: ReviewRunLog,
{
    #[must_use]
    pub fn new(knowledge: K, tasks: T, log: L) -> Self {
        Self {
            knowledge,
            tasks,
            log,
        }
    }

    /// Persists a human-editable review artifact through the knowledge
    /// provider. Replays of the same operation identity are idempotent.
    ///
    /// # Errors
    /// Provider failures leave review state untouched and surface as typed
    /// [`crate::ApplicationError`]s.
    pub async fn persist_review(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Option<ProviderMutation>, crate::ApplicationError> {
        if self.log.find(workspace_id, operation_id).await?.is_some() {
            return Ok(None);
        }
        let create = KnowledgeCreate::new(workspace_id, operation_id, title, body)
            .map_err(|error| provider_error(&error))?;
        let mutation = self
            .knowledge
            .create(create)
            .await
            .map_err(|error| provider_error(&error))?;
        self.log
            .record(ReviewRunRecord::new(
                workspace_id,
                operation_id,
                ReviewRunKind::ReviewArtifact,
                Utc::now(),
            ))
            .await?;
        Ok(Some(mutation))
    }

    /// Persists an accepted follow-up commitment as a provider task with a
    /// stable identity. Replays of the same operation identity are
    /// idempotent.
    ///
    /// # Errors
    /// Provider failures leave review state untouched and surface as typed
    /// [`crate::ApplicationError`]s.
    #[allow(clippy::too_many_arguments)]
    pub async fn accept_follow_up(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        task_id: TaskId,
        title: impl Into<String>,
        body: impl Into<String>,
        priority: ProviderTaskPriority,
        scheduling: TaskSchedulingMetadata,
    ) -> Result<Option<ProviderMutation>, crate::ApplicationError> {
        if self.log.find(workspace_id, operation_id).await?.is_some() {
            return Ok(None);
        }
        let create = TaskCreate::new(
            workspace_id,
            operation_id,
            task_id,
            title,
            body,
            priority,
            scheduling,
        )
        .map_err(|error| provider_error(&error))?;
        let mutation = self
            .tasks
            .create(create)
            .await
            .map_err(|error| provider_error(&error))?;
        self.log
            .record(ReviewRunRecord::new(
                workspace_id,
                operation_id,
                ReviewRunKind::FollowUpTask,
                Utc::now(),
            ))
            .await?;
        Ok(Some(mutation))
    }
}

fn provider_error(error: &ProviderError) -> crate::ApplicationError {
    match error {
        ProviderError::Validation { field } => crate::ApplicationError::Validation { field },
        ProviderError::Unauthorized => crate::ApplicationError::PermissionDenied,
        ProviderError::NotFound { .. } => crate::ApplicationError::NotFound { entity: "document" },
        ProviderError::Conflict { .. } => crate::ApplicationError::Conflict { entity: "document" },
        ProviderError::Unavailable => {
            crate::ApplicationError::Storage("vault provider unavailable".into())
        }
        ProviderError::Internal => crate::ApplicationError::Internal,
    }
}
