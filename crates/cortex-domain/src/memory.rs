use std::collections::BTreeMap;

use crate::text::validate_text;
use crate::{DomainError, EntityId, Lifecycle, Revision, SourceRef, WorkspaceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryStatus {
    Active,
    Superseded,
    Forgotten,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryAssertionInput {
    pub workspace_id: WorkspaceId,
    pub statement: String,
    pub normalized_subject: String,
    pub normalized_predicate: String,
    pub normalized_object: String,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryAssertion {
    id: EntityId,
    workspace_id: WorkspaceId,
    statement: String,
    normalized_subject: String,
    normalized_predicate: String,
    normalized_object: String,
    sources: Vec<SourceRef>,
    supersedes: Option<EntityId>,
    status: MemoryStatus,
    revision: Revision,
    lifecycle: Lifecycle,
}

impl MemoryAssertion {
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the assertion is blank, its fact
    /// key is incomplete, or it has no provenance sources.
    pub fn create(input: MemoryAssertionInput) -> Result<Self, DomainError> {
        Self::from_input(input, None)
    }

    /// Restores a memory assertion from durable state while reapplying all
    /// content and provenance validation.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if assertion text or fact-key
    /// fields are blank, provenance is empty, or an assertion supersedes itself.
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        id: EntityId,
        workspace_id: WorkspaceId,
        statement: String,
        normalized_subject: String,
        normalized_predicate: String,
        normalized_object: String,
        sources: Vec<SourceRef>,
        supersedes: Option<EntityId>,
        status: MemoryStatus,
        revision: Revision,
        lifecycle: Lifecycle,
    ) -> Result<Self, DomainError> {
        validate_fields(
            &statement,
            &normalized_subject,
            &normalized_predicate,
            &normalized_object,
            &sources,
        )?;
        if supersedes == Some(id) {
            return Err(DomainError::validation(
                "supersedes",
                "assertion cannot supersede itself",
            ));
        }

        Ok(Self {
            id,
            workspace_id,
            statement,
            normalized_subject,
            normalized_predicate,
            normalized_object,
            sources,
            supersedes,
            status,
            revision,
            lifecycle,
        })
    }

    /// Creates a replacement assertion without altering this assertion.
    ///
    /// The application layer persists the predecessor's semantic status change
    /// in the same transaction as the successor.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Validation`] if the successor is invalid or
    /// belongs to another workspace.
    pub fn correct(&self, input: MemoryAssertionInput) -> Result<Self, DomainError> {
        if input.workspace_id != self.workspace_id {
            return Err(DomainError::validation(
                "workspace_id",
                "correction must remain in the original workspace",
            ));
        }
        Self::from_input(input, Some(self.id))
    }

    /// Marks the assertion as semantically forgotten while preserving its
    /// immutable evidence for explicit history queries.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RevisionOverflow`] when its revision cannot
    /// advance.
    pub fn forget(&self) -> Result<Self, DomainError> {
        Ok(Self {
            status: MemoryStatus::Forgotten,
            revision: self.revision.next()?,
            ..self.clone()
        })
    }

    fn from_input(
        input: MemoryAssertionInput,
        supersedes: Option<EntityId>,
    ) -> Result<Self, DomainError> {
        validate_fields(
            &input.statement,
            &input.normalized_subject,
            &input.normalized_predicate,
            &input.normalized_object,
            &input.sources,
        )?;

        Ok(Self {
            id: EntityId::new(),
            workspace_id: input.workspace_id,
            statement: input.statement,
            normalized_subject: input.normalized_subject,
            normalized_predicate: input.normalized_predicate,
            normalized_object: input.normalized_object,
            sources: input.sources,
            supersedes,
            status: MemoryStatus::Active,
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
    pub fn statement(&self) -> &str {
        &self.statement
    }

    #[must_use]
    pub fn normalized_subject(&self) -> &str {
        &self.normalized_subject
    }

    #[must_use]
    pub fn normalized_predicate(&self) -> &str {
        &self.normalized_predicate
    }

    #[must_use]
    pub fn normalized_object(&self) -> &str {
        &self.normalized_object
    }

    #[must_use]
    pub fn sources(&self) -> &[SourceRef] {
        &self.sources
    }

    #[must_use]
    pub const fn supersedes(&self) -> Option<EntityId> {
        self.supersedes
    }

    #[must_use]
    pub const fn status(&self) -> MemoryStatus {
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

fn validate_fields(
    statement: &str,
    normalized_subject: &str,
    normalized_predicate: &str,
    normalized_object: &str,
    sources: &[SourceRef],
) -> Result<(), DomainError> {
    validate_text("statement", statement)?;
    validate_text("normalized_subject", normalized_subject)?;
    validate_text("normalized_predicate", normalized_predicate)?;
    validate_text("normalized_object", normalized_object)?;
    if sources.is_empty() {
        return Err(DomainError::validation(
            "sources",
            "at least one provenance source is required",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictSet {
    workspace_id: WorkspaceId,
    normalized_subject: String,
    normalized_predicate: String,
    assertions: Vec<MemoryAssertion>,
}

impl ConflictSet {
    /// Groups active assertions that make competing claims for one fact key.
    #[must_use]
    pub fn from_assertions(assertions: &[MemoryAssertion]) -> Vec<Self> {
        let mut groups: BTreeMap<(WorkspaceId, &str, &str), Vec<&MemoryAssertion>> =
            BTreeMap::new();
        for assertion in assertions {
            if assertion.status == MemoryStatus::Active && assertion.lifecycle == Lifecycle::Active
            {
                groups
                    .entry((
                        assertion.workspace_id,
                        &assertion.normalized_subject,
                        &assertion.normalized_predicate,
                    ))
                    .or_default()
                    .push(assertion);
            }
        }

        groups
            .into_iter()
            .filter_map(|((workspace_id, subject, predicate), group)| {
                let distinct_objects = group
                    .iter()
                    .map(|assertion| assertion.normalized_object.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                (distinct_objects.len() > 1).then(|| Self {
                    workspace_id,
                    normalized_subject: subject.to_owned(),
                    normalized_predicate: predicate.to_owned(),
                    assertions: group.into_iter().cloned().collect(),
                })
            })
            .collect()
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub fn normalized_subject(&self) -> &str {
        &self.normalized_subject
    }

    #[must_use]
    pub fn normalized_predicate(&self) -> &str {
        &self.normalized_predicate
    }

    #[must_use]
    pub fn assertions(&self) -> &[MemoryAssertion] {
        &self.assertions
    }
}
