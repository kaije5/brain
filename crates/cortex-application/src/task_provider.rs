#![allow(clippy::missing_errors_doc, clippy::result_large_err)]

use std::num::{NonZeroU32, NonZeroUsize};

use chrono::{DateTime, Utc};
use cortex_domain::{
    ObservedRevision, OperationId, ProviderProvenance, ProviderResourceKind, ProviderResourceRef,
    TaskId, WorkspaceId,
};

use crate::provider::{
    MAX_PROVIDER_LABEL_BYTES, MAX_PROVIDER_QUERY_BYTES, MAX_PROVIDER_TEXT_BYTES, ProviderError,
    ProviderMutation, ProviderPage, ProviderRead, validate_body, validate_limit,
    validate_required_text, validate_resource_kind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderTaskStatus {
    Todo,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderTaskPriority {
    Low,
    Normal,
    High,
    Urgent,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskSchedulingMetadata {
    due_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
    duration_minutes: Option<NonZeroU32>,
    earliest_start: Option<DateTime<Utc>>,
    split: Option<bool>,
    project: Option<String>,
    context: Option<String>,
}

impl TaskSchedulingMetadata {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        due_at: Option<DateTime<Utc>>,
        deadline_at: Option<DateTime<Utc>>,
        duration_minutes: Option<NonZeroU32>,
        earliest_start: Option<DateTime<Utc>>,
        split: Option<bool>,
        project: Option<impl Into<String>>,
        context: Option<impl Into<String>>,
    ) -> Result<Self, ProviderError> {
        let project = project.map(Into::into);
        let context = context.map(Into::into);
        if let Some(value) = &project {
            validate_required_text(value, "project", MAX_PROVIDER_LABEL_BYTES)?;
        }
        if let Some(value) = &context {
            validate_required_text(value, "context", MAX_PROVIDER_LABEL_BYTES)?;
        }
        Ok(Self {
            due_at,
            deadline_at,
            duration_minutes,
            earliest_start,
            split,
            project,
            context,
        })
    }

    #[must_use]
    pub const fn due_at(&self) -> Option<DateTime<Utc>> {
        self.due_at
    }
    #[must_use]
    pub const fn deadline_at(&self) -> Option<DateTime<Utc>> {
        self.deadline_at
    }
    #[must_use]
    pub const fn duration_minutes(&self) -> Option<NonZeroU32> {
        self.duration_minutes
    }
    #[must_use]
    pub const fn earliest_start(&self) -> Option<DateTime<Utc>> {
        self.earliest_start
    }
    #[must_use]
    pub const fn split(&self) -> Option<bool> {
        self.split
    }
    #[must_use]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }
    #[must_use]
    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderTask {
    provenance: ProviderProvenance,
    task_id: TaskId,
    title: String,
    body: String,
    status: ProviderTaskStatus,
    priority: ProviderTaskPriority,
    scheduling: TaskSchedulingMetadata,
}

impl ProviderTask {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provenance: ProviderProvenance,
        task_id: TaskId,
        title: impl Into<String>,
        body: impl Into<String>,
        status: ProviderTaskStatus,
        priority: ProviderTaskPriority,
        scheduling: TaskSchedulingMetadata,
    ) -> Result<Self, ProviderError> {
        validate_resource_kind(provenance.resource(), ProviderResourceKind::Task)?;
        let title = title.into();
        let body = body.into();
        validate_required_text(&title, "title", MAX_PROVIDER_TEXT_BYTES)?;
        validate_body(&body)?;
        Ok(Self {
            provenance,
            task_id,
            title,
            body,
            status,
            priority,
            scheduling,
        })
    }

    #[must_use]
    pub const fn provenance(&self) -> &ProviderProvenance {
        &self.provenance
    }
    #[must_use]
    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
    #[must_use]
    pub const fn status(&self) -> ProviderTaskStatus {
        self.status
    }
    #[must_use]
    pub const fn priority(&self) -> ProviderTaskPriority {
        self.priority
    }
    #[must_use]
    pub const fn scheduling(&self) -> &TaskSchedulingMetadata {
        &self.scheduling
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskQuery {
    workspace_id: WorkspaceId,
    text: Option<String>,
    limit: NonZeroUsize,
}

impl TaskQuery {
    pub fn new<T: Into<String>>(
        workspace_id: WorkspaceId,
        text: Option<T>,
        limit: NonZeroUsize,
    ) -> Result<Self, ProviderError> {
        let text = text.map(Into::into);
        if let Some(value) = &text {
            validate_required_text(value, "query", MAX_PROVIDER_QUERY_BYTES)?;
        }
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
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    #[must_use]
    pub const fn limit(&self) -> NonZeroUsize {
        self.limit
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskCreate {
    workspace_id: WorkspaceId,
    operation_id: OperationId,
    task_id: TaskId,
    title: String,
    body: String,
    priority: ProviderTaskPriority,
    scheduling: TaskSchedulingMetadata,
}

impl TaskCreate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        task_id: TaskId,
        title: impl Into<String>,
        body: impl Into<String>,
        priority: ProviderTaskPriority,
        scheduling: TaskSchedulingMetadata,
    ) -> Result<Self, ProviderError> {
        let title = title.into();
        let body = body.into();
        validate_required_text(&title, "title", MAX_PROVIDER_TEXT_BYTES)?;
        validate_body(&body)?;
        Ok(Self {
            workspace_id,
            operation_id,
            task_id,
            title,
            body,
            priority,
            scheduling,
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
    pub const fn task_id(&self) -> TaskId {
        self.task_id
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
    #[must_use]
    pub const fn priority(&self) -> ProviderTaskPriority {
        self.priority
    }
    #[must_use]
    pub const fn scheduling(&self) -> &TaskSchedulingMetadata {
        &self.scheduling
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskUpdate {
    resource: ProviderResourceRef,
    operation_id: OperationId,
    expected_revision: ObservedRevision,
    title: String,
    body: String,
    status: ProviderTaskStatus,
    priority: ProviderTaskPriority,
    scheduling: TaskSchedulingMetadata,
}

impl TaskUpdate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        resource: ProviderResourceRef,
        operation_id: OperationId,
        expected_revision: ObservedRevision,
        title: impl Into<String>,
        body: impl Into<String>,
        status: ProviderTaskStatus,
        priority: ProviderTaskPriority,
        scheduling: TaskSchedulingMetadata,
    ) -> Result<Self, ProviderError> {
        validate_resource_kind(&resource, ProviderResourceKind::Task)?;
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
            status,
            priority,
            scheduling,
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
    #[must_use]
    pub const fn status(&self) -> ProviderTaskStatus {
        self.status
    }
    #[must_use]
    pub const fn priority(&self) -> ProviderTaskPriority {
        self.priority
    }
    #[must_use]
    pub const fn scheduling(&self) -> &TaskSchedulingMetadata {
        &self.scheduling
    }
}

macro_rules! existing_task_input {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name {
            resource: ProviderResourceRef,
            operation_id: OperationId,
            expected_revision: ObservedRevision,
        }

        impl $name {
            pub fn new(
                resource: ProviderResourceRef,
                operation_id: OperationId,
                expected_revision: ObservedRevision,
            ) -> Result<Self, ProviderError> {
                validate_resource_kind(&resource, ProviderResourceKind::Task)?;
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
    };
}

existing_task_input!(TaskComplete);
existing_task_input!(TaskDelete);

#[allow(async_fn_in_trait)]
pub trait TaskProvider: Send + Sync {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError>;
    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError>;
    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError>;
    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError>;
    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError>;
    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError>;
}
