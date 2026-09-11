#![allow(clippy::missing_errors_doc, clippy::result_large_err)]

use std::num::NonZeroUsize;

use cortex_domain::{
    ObservedRevision, OperationId, ProviderProvenance, ProviderResourceKind, ProviderResourceRef,
    WorkspaceId,
};

use crate::provider::{
    MAX_PROVIDER_QUERY_BYTES, MAX_PROVIDER_TEXT_BYTES, ProviderError, ProviderMutation,
    ProviderPage, ProviderRead, validate_body, validate_limit, validate_required_text,
    validate_resource_kind,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeDocument {
    provenance: ProviderProvenance,
    title: String,
    body: String,
}

impl KnowledgeDocument {
    pub fn new(
        provenance: ProviderProvenance,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        validate_resource_kind(provenance.resource(), ProviderResourceKind::Knowledge)?;
        let title = title.into();
        let body = body.into();
        validate_required_text(&title, "title", MAX_PROVIDER_TEXT_BYTES)?;
        validate_body(&body)?;
        Ok(Self {
            provenance,
            title,
            body,
        })
    }

    #[must_use]
    pub const fn provenance(&self) -> &ProviderProvenance {
        &self.provenance
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeQuery {
    workspace_id: WorkspaceId,
    text: String,
    limit: NonZeroUsize,
}

impl KnowledgeQuery {
    pub fn new(
        workspace_id: WorkspaceId,
        text: impl Into<String>,
        limit: NonZeroUsize,
    ) -> Result<Self, ProviderError> {
        let text = text.into();
        validate_required_text(&text, "query", MAX_PROVIDER_QUERY_BYTES)?;
        validate_limit(limit)?;
        Ok(Self {
            workspace_id,
            text,
            limit,
        })
    }

    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn limit(&self) -> NonZeroUsize {
        self.limit
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeCreate {
    workspace_id: WorkspaceId,
    operation_id: OperationId,
    title: String,
    body: String,
}

impl KnowledgeCreate {
    pub fn new(
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let title = title.into();
        let body = body.into();
        validate_required_text(&title, "title", MAX_PROVIDER_TEXT_BYTES)?;
        validate_body(&body)?;
        Ok(Self {
            workspace_id,
            operation_id,
            title,
            body,
        })
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
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeUpdate {
    resource: ProviderResourceRef,
    operation_id: OperationId,
    expected_revision: ObservedRevision,
    title: String,
    body: String,
}

impl KnowledgeUpdate {
    pub fn new(
        resource: ProviderResourceRef,
        operation_id: OperationId,
        expected_revision: ObservedRevision,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        validate_resource_kind(&resource, ProviderResourceKind::Knowledge)?;
        let title = title.into();
        let body = body.into();
        validate_required_text(&title, "title", MAX_PROVIDER_TEXT_BYTES)?;
        validate_body(&body)?;
        Ok(Self {
            resource,
            operation_id,
            expected_revision,
            title,
            body,
        })
    }

    #[must_use]
    pub const fn resource(&self) -> &ProviderResourceRef {
        &self.resource
    }
    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }
    #[must_use]
    pub const fn expected_revision(&self) -> &ObservedRevision {
        &self.expected_revision
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeDelete {
    resource: ProviderResourceRef,
    operation_id: OperationId,
    expected_revision: ObservedRevision,
}

impl KnowledgeDelete {
    pub fn new(
        resource: ProviderResourceRef,
        operation_id: OperationId,
        expected_revision: ObservedRevision,
    ) -> Result<Self, ProviderError> {
        validate_resource_kind(&resource, ProviderResourceKind::Knowledge)?;
        Ok(Self {
            resource,
            operation_id,
            expected_revision,
        })
    }

    #[must_use]
    pub const fn resource(&self) -> &ProviderResourceRef {
        &self.resource
    }
    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
    }
    #[must_use]
    pub const fn expected_revision(&self) -> &ObservedRevision {
        &self.expected_revision
    }
}

#[allow(async_fn_in_trait)]
pub trait KnowledgeProvider: Send + Sync {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError>;
    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError>;
    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError>;
    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError>;
    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError>;
}
