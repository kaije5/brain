use crate::EntityId;
use crate::text::validate_text;
use crate::{DomainError, Lifecycle, Revision, WorkspaceId};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceRef {
    pub source_id: EntityId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceInput {
    pub workspace_id: WorkspaceId,
    pub reference: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    id: EntityId,
    workspace_id: WorkspaceId,
    reference: String,
    revision: Revision,
    lifecycle: Lifecycle,
}

impl Source {
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the reference is blank.
    pub fn create(input: SourceInput) -> Result<Self, DomainError> {
        validate_text("reference", &input.reference)?;

        Ok(Self {
            id: EntityId::new(),
            workspace_id: input.workspace_id,
            reference: input.reference,
            revision: Revision::initial(),
            lifecycle: Lifecycle::Active,
        })
    }

    /// Restores immutable source evidence from durable state while reapplying validation.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the reference is blank.
    pub fn rehydrate(
        id: EntityId,
        workspace_id: WorkspaceId,
        reference: String,
        revision: Revision,
        lifecycle: Lifecycle,
    ) -> Result<Self, DomainError> {
        validate_text("reference", &reference)?;

        Ok(Self {
            id,
            workspace_id,
            reference,
            revision,
            lifecycle,
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
    pub fn reference(&self) -> &str {
        &self.reference
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
