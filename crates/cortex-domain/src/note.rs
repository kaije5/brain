use crate::{DomainError, EntityId, Lifecycle, Revision, WorkspaceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteInput {
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note {
    id: EntityId,
    workspace_id: WorkspaceId,
    title: String,
    content: String,
    revision: Revision,
    lifecycle: Lifecycle,
}

impl Note {
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if title or content is blank.
    pub fn create(input: NoteInput) -> Result<Self, DomainError> {
        validate_text("title", &input.title)?;
        validate_text("content", &input.content)?;

        Ok(Self {
            id: EntityId::new(),
            workspace_id: input.workspace_id,
            title: input.title,
            content: input.content,
            revision: Revision::initial(),
            lifecycle: Lifecycle::Active,
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
    pub fn content(&self) -> &str {
        &self.content
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

pub(crate) fn validate_text(field: &'static str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::validation(field, "must not be blank"));
    }
    Ok(())
}
