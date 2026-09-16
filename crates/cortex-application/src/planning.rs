//! Deterministic planning over provider-backed tasks (SCRUM-128/129).
//!
//! The planner consumes [`ProviderTask`]s read through [`TaskProvider`] — the
//! Markdown vault is the task authority — and never assumes a canonical
//! SQLite task row. Work-block intent and calendar linkage are Cortex-owned
//! runtime state recorded through [`WorkBlockLedger`], keyed by the stable
//! [`TaskId`] (`brain_id`), so vault renames and file moves never break an
//! existing linkage.
//!
//! Failure semantics: a provider read failure aborts before any Cortex state
//! is written, and the ledger write happens only after planning succeeds, so
//! failed or conflicting provider operations never advance planner state.

use std::num::NonZeroU32;

use chrono::{DateTime, Duration, Utc};
use cortex_domain::{TaskId, WorkspaceId};

use crate::{
    ApplicationError, ProviderError, ProviderTask, ProviderTaskPriority, ProviderTaskStatus,
    TaskProvider, TaskQuery,
};

/// Default work-block length when a task carries no explicit duration.
pub const DEFAULT_WORK_BLOCK_MINUTES: u32 = 30;

/// Cortex-owned semantic label for why a work block exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkBlockIntent {
    Deep,
    Admin,
    Review,
}

/// A scheduled unit of work bound to a provider task by stable identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedWorkBlock {
    task_id: TaskId,
    intent: WorkBlockIntent,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
}

impl PlannedWorkBlock {
    #[must_use]
    pub const fn new(
        task_id: TaskId,
        intent: WorkBlockIntent,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
    ) -> Self {
        Self {
            task_id,
            intent,
            starts_at,
            ends_at,
        }
    }

    #[must_use]
    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    #[must_use]
    pub const fn intent(&self) -> WorkBlockIntent {
        self.intent
    }

    #[must_use]
    pub const fn starts_at(&self) -> DateTime<Utc> {
        self.starts_at
    }

    #[must_use]
    pub const fn ends_at(&self) -> DateTime<Utc> {
        self.ends_at
    }
}

/// Cortex-owned record binding a work block to a calendar event. Keyed by
/// the stable [`TaskId`]; task renames and moves never participate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkBlockLinkage {
    workspace_id: WorkspaceId,
    task_id: TaskId,
    calendar_event_id: String,
}

impl WorkBlockLinkage {
    pub fn new(
        workspace_id: WorkspaceId,
        task_id: TaskId,
        calendar_event_id: impl Into<String>,
    ) -> Result<Self, ApplicationError> {
        let calendar_event_id = calendar_event_id.into();
        if calendar_event_id.trim().is_empty()
            || calendar_event_id.len() > 256
            || calendar_event_id.chars().any(char::is_control)
        {
            return Err(ApplicationError::Validation {
                field: "calendar_event_id",
            });
        }
        Ok(Self {
            workspace_id,
            task_id,
            calendar_event_id,
        })
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }

    #[must_use]
    pub fn calendar_event_id(&self) -> &str {
        &self.calendar_event_id
    }
}

/// Cortex-owned storage for work-block intent and calendar linkage. This is
/// runtime state: the vault stores tasks, Cortex stores what Brain planned
/// against them.
#[allow(async_fn_in_trait)]
pub trait WorkBlockLedger: Send + Sync {
    /// Inserts or replaces the linkage for the block's task.
    async fn record(&self, linkage: WorkBlockLinkage) -> Result<(), ApplicationError>;
    /// Resolves the linkage for a task by stable identity, if any.
    async fn find(&self, task_id: TaskId) -> Result<Option<WorkBlockLinkage>, ApplicationError>;
    /// Removes every linkage recorded for the workspace (re-plan boundary).
    async fn clear(&self, workspace_id: WorkspaceId) -> Result<(), ApplicationError>;
}

/// A bounded scheduling window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanningWindow {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

impl PlanningWindow {
    #[must_use]
    pub const fn new(starts_at: DateTime<Utc>, ends_at: DateTime<Utc>) -> Self {
        Self { starts_at, ends_at }
    }
}

/// Ranks a task for scheduling. Deterministic: hard deadlines before soft due
/// dates before open-ended work; higher priority first; [`TaskId`] (UUIDv7)
/// as the final stable tiebreak.
fn scheduling_rank(task: &ProviderTask) -> (chrono::DateTime<Utc>, u8, cortex_domain::TaskId) {
    let deadline = task
        .scheduling()
        .deadline_at()
        .or(task.scheduling().due_at());
    let priority = match task.priority() {
        ProviderTaskPriority::Urgent => 3,
        ProviderTaskPriority::High => 2,
        ProviderTaskPriority::Normal => 1,
        ProviderTaskPriority::Low => 0,
    };
    (
        deadline.unwrap_or(DateTime::<Utc>::MAX_UTC),
        priority,
        task.task_id(),
    )
}

/// Derives the Cortex-owned work-block intent for a task.
#[must_use]
pub fn work_block_intent(task: &ProviderTask) -> WorkBlockIntent {
    match (task.scheduling().context(), task.priority()) {
        (Some("admin"), _) | (_, ProviderTaskPriority::Low) => WorkBlockIntent::Admin,
        (_, ProviderTaskPriority::Urgent | ProviderTaskPriority::High) => WorkBlockIntent::Deep,
        _ => WorkBlockIntent::Review,
    }
}

/// Deterministically schedules eligible tasks into sequential, non-overlapping
/// work blocks inside the window. Pure: no I/O, no clock reads — identical
/// inputs always produce identical blocks.
#[must_use]
pub fn plan_schedule(tasks: &[ProviderTask], window: PlanningWindow) -> Vec<PlannedWorkBlock> {
    let mut eligible: Vec<&ProviderTask> = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status(),
                ProviderTaskStatus::Todo | ProviderTaskStatus::InProgress
            )
        })
        .filter(|task| {
            task.scheduling()
                .earliest_start()
                .is_none_or(|earliest| earliest <= window.ends_at)
        })
        .collect();
    eligible.sort_by_key(|task| scheduling_rank(task));

    let mut blocks = Vec::new();
    let mut cursor = window.starts_at;
    for task in eligible {
        let minutes = task
            .scheduling()
            .duration_minutes()
            .map_or(DEFAULT_WORK_BLOCK_MINUTES, NonZeroU32::get);
        let duration = Duration::minutes(i64::from(minutes));
        let starts_at = cursor.max(
            task.scheduling()
                .earliest_start()
                .unwrap_or(window.starts_at),
        );
        let ends_at = starts_at + duration;
        if ends_at > window.ends_at {
            continue;
        }
        blocks.push(PlannedWorkBlock::new(
            task.task_id(),
            work_block_intent(task),
            starts_at,
            ends_at,
        ));
        cursor = ends_at;
    }
    blocks
}

/// Provider-backed planning boundary: reads tasks through [`TaskProvider`],
/// plans deterministically, and records Cortex-owned linkage only after a
/// successful plan. Provider failures leave the ledger untouched.
pub struct PlanningService<T, L> {
    tasks: T,
    ledger: L,
    plan_limit: std::num::NonZeroUsize,
}

impl<T, L> PlanningService<T, L>
where
    T: TaskProvider,
    L: WorkBlockLedger,
{
    #[must_use]
    pub fn new(tasks: T, ledger: L) -> Self {
        Self {
            tasks,
            ledger,
            plan_limit: std::num::NonZeroUsize::new(crate::MAX_PROVIDER_RESULTS)
                .unwrap_or(std::num::NonZeroUsize::MIN),
        }
    }

    /// Reads provider-backed tasks, plans the window, and persists one
    /// linkage per planned block. Returns the planned blocks.
    ///
    /// # Errors
    /// Provider read/mutation failures and ledger storage failures are typed
    /// [`ApplicationError`]s; planner state advances only on full success.
    pub async fn plan_window(
        &self,
        workspace_id: WorkspaceId,
        window: PlanningWindow,
    ) -> Result<Vec<PlannedWorkBlock>, ApplicationError> {
        let query = TaskQuery::new(workspace_id, Option::<String>::None, self.plan_limit)
            .map_err(provider_error)?;
        let page = self.tasks.search(&query).await.map_err(provider_error)?;
        let blocks = plan_schedule(page.items(), window);
        self.ledger.clear(workspace_id).await?;
        for block in &blocks {
            self.ledger
                .record(WorkBlockLinkage::new(
                    workspace_id,
                    block.task_id(),
                    block.starts_at().to_rfc3339(),
                )?)
                .await?;
        }
        Ok(blocks)
    }

    /// Resolves the recorded linkage for a task by stable identity.
    ///
    /// # Errors
    /// Ledger storage failures are typed [`ApplicationError`]s.
    pub async fn linkage_for(
        &self,
        task_id: TaskId,
    ) -> Result<Option<WorkBlockLinkage>, ApplicationError> {
        self.ledger.find(task_id).await
    }
}

fn provider_error(error: ProviderError) -> ApplicationError {
    match error {
        ProviderError::Validation { field } => ApplicationError::Validation { field },
        ProviderError::Unauthorized => ApplicationError::PermissionDenied,
        ProviderError::NotFound { .. } => ApplicationError::NotFound { entity: "task" },
        ProviderError::Conflict { .. } => ApplicationError::Conflict { entity: "task" },
        ProviderError::Unavailable => ApplicationError::Storage("task provider unavailable".into()),
        ProviderError::Internal => ApplicationError::Internal,
    }
}
