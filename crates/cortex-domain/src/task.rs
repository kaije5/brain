use chrono::{DateTime, Utc};

use crate::{DomainError, EntityId, Lifecycle, Revision, WorkspaceId, note::validate_text};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskInput {
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskStatus {
    Open,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    id: EntityId,
    workspace_id: WorkspaceId,
    title: String,
    due_at: Option<DateTime<Utc>>,
    status: TaskStatus,
    revision: Revision,
    lifecycle: Lifecycle,
}

impl Task {
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the title is blank.
    pub fn create(input: TaskInput) -> Result<Self, DomainError> {
        validate_text("title", &input.title)?;

        Ok(Self {
            id: EntityId::new(),
            workspace_id: input.workspace_id,
            title: input.title,
            due_at: input.due_at,
            status: TaskStatus::Open,
            revision: Revision::initial(),
            lifecycle: Lifecycle::Active,
        })
    }

    /// Restores a task from durable state while reapplying domain validation.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the title is blank.
    pub fn rehydrate(
        id: EntityId,
        workspace_id: WorkspaceId,
        title: String,
        due_at: Option<DateTime<Utc>>,
        status: TaskStatus,
        revision: Revision,
        lifecycle: Lifecycle,
    ) -> Result<Self, DomainError> {
        validate_text("title", &title)?;

        Ok(Self {
            id,
            workspace_id,
            title,
            due_at,
            status,
            revision,
            lifecycle,
        })
    }

    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] when the task is already completed,
    /// or [`DomainError::RevisionOverflow`] when its revision cannot advance.
    pub fn complete(&self) -> Result<Self, DomainError> {
        if self.status == TaskStatus::Completed {
            return Err(DomainError::validation(
                "status",
                "task is already completed",
            ));
        }

        Ok(Self {
            status: TaskStatus::Completed,
            revision: self.revision.next()?,
            ..self.clone()
        })
    }

    #[must_use]
    pub const fn id(&self) -> EntityId {
        self.id
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub const fn due_at(&self) -> Option<DateTime<Utc>> {
        self.due_at
    }

    #[must_use]
    pub const fn status(&self) -> TaskStatus {
        self.status
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
}
