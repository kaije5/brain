use crate::{DomainError, WorkspaceId};

const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_REVISION_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(String);

impl ProviderId {
    /// Creates a bounded provider identifier.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the value is blank, contains control
    /// characters, or exceeds the provider identifier bound.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        validate_opaque(value.into(), "provider_id", MAX_PROVIDER_ID_BYTES).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderResourceId(String);

impl ProviderResourceId {
    /// Creates a bounded provider-owned resource identifier.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the value is blank, contains control
    /// characters, or exceeds the resource identifier bound.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        validate_opaque(value.into(), "provider_resource_id", MAX_RESOURCE_ID_BYTES).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservedRevision(String);

impl ObservedRevision {
    /// Creates a bounded opaque provider revision.
    ///
    /// # Errors
    ///
    /// Returns a validation error when the value is blank, contains control
    /// characters, or exceeds the revision bound.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        validate_opaque(value.into(), "observed_revision", MAX_REVISION_BYTES).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    #[must_use]
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProviderResourceKind {
    Knowledge,
    Task,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderResourceRef {
    workspace_id: WorkspaceId,
    provider_id: ProviderId,
    resource_id: ProviderResourceId,
    kind: ProviderResourceKind,
}

impl ProviderResourceRef {
    #[must_use]
    pub const fn new(
        workspace_id: WorkspaceId,
        provider_id: ProviderId,
        resource_id: ProviderResourceId,
        kind: ProviderResourceKind,
    ) -> Self {
        Self {
            workspace_id,
            provider_id,
            resource_id,
            kind,
        }
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    #[must_use]
    pub const fn resource_id(&self) -> &ProviderResourceId {
        &self.resource_id
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderResourceKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProvenance {
    resource: ProviderResourceRef,
    observed_revision: ObservedRevision,
    content_hash: ContentHash,
}

impl ProviderProvenance {
    #[must_use]
    pub const fn new(
        resource: ProviderResourceRef,
        observed_revision: ObservedRevision,
        content_hash: ContentHash,
    ) -> Self {
        Self {
            resource,
            observed_revision,
            content_hash,
        }
    }

    #[must_use]
    pub const fn resource(&self) -> &ProviderResourceRef {
        &self.resource
    }

    #[must_use]
    pub const fn observed_revision(&self) -> &ObservedRevision {
        &self.observed_revision
    }

    #[must_use]
    pub const fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }
}

fn validate_opaque(
    value: String,
    field: &'static str,
    max_bytes: usize,
) -> Result<String, DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::validation(field, "must not be blank"));
    }
    if value.len() > max_bytes {
        return Err(DomainError::validation(
            field,
            format!("must be at most {max_bytes} bytes"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(DomainError::validation(
            field,
            "must not contain control characters",
        ));
    }
    Ok(value)
}
