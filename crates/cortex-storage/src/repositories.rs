use chrono::{DateTime, Utc};
use cortex_application::{
    ApplicationError, MemoryRepository, NoteRepository, SourceRepository, TaskRepository,
};
use cortex_domain::{
    EntityId, Lifecycle, MemoryAssertion, MemoryStatus, Note, PrincipalId, Revision, Source,
    SourceRef, Task, TaskStatus, WorkspaceId,
};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::database::storage_error;

#[derive(Clone)]
pub struct SqliteRepositories {
    pool: SqlitePool,
}

impl SqliteRepositories {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates an explicit workspace ownership boundary.
    ///
    /// # Errors
    ///
    /// Returns a validation error for a blank name or a redacted storage error on conflict.
    pub async fn create_workspace(
        &self,
        workspace_id: WorkspaceId,
        name: &str,
    ) -> Result<(), ApplicationError> {
        validate_name(name)?;
        sqlx::query("INSERT INTO workspace (id, name) VALUES (?, ?)")
            .bind(id_text(workspace_id))
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|_| storage_error("workspace insert failed"))?;
        Ok(())
    }

    /// Creates an authenticated principal within an existing workspace.
    ///
    /// # Errors
    ///
    /// Returns a validation error for a blank name or a redacted storage error on conflict.
    pub async fn create_principal(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        name: &str,
    ) -> Result<(), ApplicationError> {
        validate_name(name)?;
        sqlx::query("INSERT INTO principal (id, workspace_id, name) VALUES (?, ?, ?)")
            .bind(id_text(principal_id))
            .bind(id_text(workspace_id))
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|_| storage_error("principal insert failed"))?;
        Ok(())
    }
}

impl NoteRepository for SqliteRepositories {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, title, content, revision, lifecycle \
              FROM note WHERE workspace_id = ? AND id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("note lookup failed"))?;
        row.map(|row| decode_note(&row)).transpose()
    }
}

impl TaskRepository for SqliteRepositories {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Task>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, title, due_at, status, revision, lifecycle \
             FROM task WHERE workspace_id = ? AND id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("task lookup failed"))?;
        row.map(|row| decode_task(&row)).transpose()
    }
}

impl SourceRepository for SqliteRepositories {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Source>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, reference, revision, lifecycle \
             FROM source WHERE workspace_id = ? AND id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("source lookup failed"))?;
        row.map(|row| decode_source(&row)).transpose()
    }
}

impl MemoryRepository for SqliteRepositories {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<MemoryAssertion>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, statement, normalized_subject, normalized_predicate, \
             normalized_object, supersedes_id, status, revision, lifecycle \
             FROM memory_assertion WHERE workspace_id = ? AND id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("memory lookup failed"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let source_ids: Vec<String> = sqlx::query_scalar(
            "SELECT source_id FROM memory_source \
             WHERE workspace_id = ? AND memory_id = ? ORDER BY source_id",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("memory provenance lookup failed"))?;
        let sources = source_ids
            .iter()
            .map(|source_id| parse_id(source_id).map(|source_id| SourceRef { source_id }))
            .collect::<Result<Vec<_>, _>>()?;
        decode_memory(&row, sources).map(Some)
    }
}

fn decode_note(row: &sqlx::sqlite::SqliteRow) -> Result<Note, ApplicationError> {
    Note::rehydrate(
        row_id(row, "id")?,
        row_id(row, "workspace_id")?,
        row_text(row, "title")?,
        row_text(row, "content")?,
        row_revision(row)?,
        decode_lifecycle(&row_text(row, "lifecycle")?)?,
    )
    .map_err(ApplicationError::from)
}

fn decode_task(row: &sqlx::sqlite::SqliteRow) -> Result<Task, ApplicationError> {
    let due_at: Option<String> = row
        .try_get("due_at")
        .map_err(|_| storage_error("invalid task row"))?;
    let due_at = due_at
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| storage_error("invalid task due date"))
        })
        .transpose()?;
    Task::rehydrate(
        row_id(row, "id")?,
        row_id(row, "workspace_id")?,
        row_text(row, "title")?,
        due_at,
        decode_task_status(&row_text(row, "status")?)?,
        row_revision(row)?,
        decode_lifecycle(&row_text(row, "lifecycle")?)?,
    )
    .map_err(ApplicationError::from)
}

fn decode_source(row: &sqlx::sqlite::SqliteRow) -> Result<Source, ApplicationError> {
    Source::rehydrate(
        row_id(row, "id")?,
        row_id(row, "workspace_id")?,
        row_text(row, "reference")?,
        row_revision(row)?,
        decode_lifecycle(&row_text(row, "lifecycle")?)?,
    )
    .map_err(ApplicationError::from)
}

fn decode_memory(
    row: &sqlx::sqlite::SqliteRow,
    sources: Vec<SourceRef>,
) -> Result<MemoryAssertion, ApplicationError> {
    let supersedes: Option<String> = row
        .try_get("supersedes_id")
        .map_err(|_| storage_error("invalid memory row"))?;
    MemoryAssertion::rehydrate(
        row_id(row, "id")?,
        row_id(row, "workspace_id")?,
        row_text(row, "statement")?,
        row_text(row, "normalized_subject")?,
        row_text(row, "normalized_predicate")?,
        row_text(row, "normalized_object")?,
        sources,
        supersedes.map(|value| parse_id(&value)).transpose()?,
        decode_memory_status(&row_text(row, "status")?)?,
        row_revision(row)?,
        decode_lifecycle(&row_text(row, "lifecycle")?)?,
    )
    .map_err(ApplicationError::from)
}

fn validate_name(name: &str) -> Result<(), ApplicationError> {
    if name.trim().is_empty() {
        return Err(ApplicationError::Validation { field: "name" });
    }
    Ok(())
}

pub(crate) fn id_text<T>(value: T) -> String
where
    Uuid: From<T>,
{
    Uuid::from(value).to_string()
}

pub(crate) fn parse_id<T>(value: &str) -> Result<T, ApplicationError>
where
    T: TryFrom<Uuid, Error = cortex_domain::DomainError>,
{
    let uuid = Uuid::parse_str(value).map_err(|_| storage_error("invalid persisted id"))?;
    T::try_from(uuid).map_err(ApplicationError::from)
}

fn row_id<T>(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<T, ApplicationError>
where
    T: TryFrom<Uuid, Error = cortex_domain::DomainError>,
{
    parse_id(&row_text(row, column)?)
}

fn row_text(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<String, ApplicationError> {
    row.try_get(column)
        .map_err(|_| storage_error("invalid repository row"))
}

fn row_revision(row: &sqlx::sqlite::SqliteRow) -> Result<Revision, ApplicationError> {
    let value: i64 = row
        .try_get("revision")
        .map_err(|_| storage_error("invalid repository row"))?;
    let value = u64::try_from(value).map_err(|_| storage_error("invalid persisted revision"))?;
    Revision::rehydrate(value).map_err(ApplicationError::from)
}

pub(crate) const fn encode_lifecycle(value: Lifecycle) -> &'static str {
    match value {
        Lifecycle::Active => "active",
        Lifecycle::Deleted => "deleted",
    }
}

pub(crate) fn decode_lifecycle(value: &str) -> Result<Lifecycle, ApplicationError> {
    match value {
        "active" => Ok(Lifecycle::Active),
        "deleted" => Ok(Lifecycle::Deleted),
        _ => Err(storage_error("invalid lifecycle")),
    }
}

pub(crate) const fn encode_task_status(value: TaskStatus) -> &'static str {
    match value {
        TaskStatus::Open => "open",
        TaskStatus::Completed => "completed",
    }
}

fn decode_task_status(value: &str) -> Result<TaskStatus, ApplicationError> {
    match value {
        "open" => Ok(TaskStatus::Open),
        "completed" => Ok(TaskStatus::Completed),
        _ => Err(storage_error("invalid task status")),
    }
}

pub(crate) const fn encode_memory_status(value: MemoryStatus) -> &'static str {
    match value {
        MemoryStatus::Active => "active",
        MemoryStatus::Superseded => "superseded",
        MemoryStatus::Forgotten => "forgotten",
    }
}

fn decode_memory_status(value: &str) -> Result<MemoryStatus, ApplicationError> {
    match value {
        "active" => Ok(MemoryStatus::Active),
        "superseded" => Ok(MemoryStatus::Superseded),
        "forgotten" => Ok(MemoryStatus::Forgotten),
        _ => Err(storage_error("invalid memory status")),
    }
}
