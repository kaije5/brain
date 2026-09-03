use chrono::{DateTime, Utc};
use cortex_application::{
    ApplicationError, Capability, Embedding, EntityKind, IndexedVector, MemoryRepository,
    NoteRepository, SearchCandidate, SearchIndex, SourceRepository, TaskRepository,
};
use cortex_domain::{
    EntityId, Lifecycle, MemoryAssertion, MemoryStatus, Note, PrincipalId, Revision, Source,
    SourceRef, Task, TaskStatus, WorkspaceId,
};
use sha2::{Digest, Sha256};
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

    /// Atomically establishes the durable owner boundary and its explicit grants.
    ///
    /// # Errors
    /// Returns a redacted storage error if bootstrap cannot be committed.
    pub async fn bootstrap_owner(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        grants: &[Capability],
    ) -> Result<(), ApplicationError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| storage_error("bootstrap failed"))?;
        sqlx::query("INSERT INTO workspace (id, name) VALUES (?, ?) ON CONFLICT (id) DO NOTHING")
            .bind(id_text(workspace_id))
            .bind("default")
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error("bootstrap failed"))?;
        sqlx::query("INSERT INTO principal (id, workspace_id, name) VALUES (?, ?, ?) ON CONFLICT (id) DO NOTHING")
            .bind(id_text(principal_id)).bind(id_text(workspace_id)).bind("owner")
            .execute(&mut *transaction).await.map_err(|_| storage_error("bootstrap failed"))?;
        for capability in grants {
            sqlx::query("INSERT INTO capability_grant (workspace_id, principal_id, capability) VALUES (?, ?, ?) ON CONFLICT DO NOTHING")
                .bind(id_text(workspace_id)).bind(id_text(principal_id)).bind(capability.metadata().mcp_name)
                .execute(&mut *transaction).await.map_err(|_| storage_error("bootstrap failed"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| storage_error("bootstrap failed"))
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

    /// Stores an explicit capability grant for an authenticated principal.
    ///
    /// # Errors
    /// Returns a redacted storage error when the referenced ownership boundary
    /// does not exist or the grant cannot be stored.
    pub async fn grant_capability(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        capability: Capability,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "INSERT INTO capability_grant (workspace_id, principal_id, capability) \
             VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(principal_id))
        .bind(capability.metadata().mcp_name)
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("capability grant insert failed"))?;
        Ok(())
    }

    /// Checks one exact stored grant inside its workspace boundary.
    ///
    /// # Errors
    /// Returns a redacted storage error when the lookup fails.
    pub async fn has_capability(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        capability: Capability,
    ) -> Result<bool, ApplicationError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM capability_grant \
             WHERE workspace_id = ? AND principal_id = ? AND capability = ?)",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(principal_id))
        .bind(capability.metadata().mcp_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| storage_error("capability grant lookup failed"))
    }

    /// Upserts searchable content and its canonical SHA-256 hash only for an
    /// existing canonical entity. A changed hash atomically invalidates vectors.
    ///
    /// # Errors
    /// Returns a validation error for blank text, not-found for a missing entity,
    /// or a redacted storage error when indexing fails.
    pub async fn upsert_search_document(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        kind: EntityKind,
        snippet: &str,
    ) -> Result<(), ApplicationError> {
        if snippet.trim().is_empty() {
            return Err(ApplicationError::Validation { field: "snippet" });
        }
        if !self
            .canonical_entity_exists(workspace_id, entity_id, kind)
            .await?
        {
            return Err(ApplicationError::NotFound { entity: "entity" });
        }
        let content_hash = search_content_hash(snippet);
        sqlx::query(
            "INSERT INTO search_document \
             (workspace_id, entity_id, entity_kind, snippet, content_hash) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (workspace_id, entity_id) DO UPDATE SET \
             entity_kind = excluded.entity_kind, snippet = excluded.snippet, \
             content_hash = excluded.content_hash, \
             updated_at = CURRENT_TIMESTAMP",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .bind(kind.as_str())
        .bind(snippet)
        .bind(content_hash.to_vec())
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("search document upsert failed"))?;
        Ok(())
    }

    /// Returns bounded lexical candidates after workspace, grant, lifecycle,
    /// memory-status, and provenance filtering.
    ///
    /// # Errors
    /// Returns a validation error for a blank query or a redacted storage error.
    pub async fn lexical_search_candidates(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &str,
        limit: std::num::NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError> {
        let fts_query = phrase_query(query)?;
        let rows = sqlx::query(
            "SELECT d.entity_id, d.entity_kind, d.snippet \
             FROM search_document_fts \
             JOIN search_document AS d ON d.row_id = search_document_fts.rowid \
             WHERE search_document_fts MATCH ? AND d.workspace_id = ? \
             AND EXISTS (SELECT 1 FROM capability_grant AS grant_row \
                 WHERE grant_row.workspace_id = d.workspace_id \
                 AND grant_row.principal_id = ? AND grant_row.capability = ?) \
             AND ((d.entity_kind = 'note' AND EXISTS (SELECT 1 FROM note AS n \
                     WHERE n.workspace_id = d.workspace_id AND n.id = d.entity_id \
                     AND n.lifecycle = 'active')) \
                 OR (d.entity_kind = 'task' AND EXISTS (SELECT 1 FROM task AS t \
                     WHERE t.workspace_id = d.workspace_id AND t.id = d.entity_id \
                     AND t.lifecycle = 'active')) \
                 OR (d.entity_kind = 'memory' AND EXISTS (SELECT 1 FROM memory_assertion AS m \
                     WHERE m.workspace_id = d.workspace_id AND m.id = d.entity_id \
                     AND m.lifecycle = 'active' AND m.status = 'active') \
                     AND EXISTS (SELECT 1 FROM memory_source AS ms \
                         JOIN source AS provenance_source \
                           ON provenance_source.workspace_id = ms.workspace_id \
                          AND provenance_source.id = ms.source_id \
                         WHERE ms.workspace_id = d.workspace_id \
                           AND ms.memory_id = d.entity_id \
                           AND provenance_source.lifecycle = 'active')) \
                 OR (d.entity_kind = 'source' AND EXISTS (SELECT 1 FROM source AS s \
                     WHERE s.workspace_id = d.workspace_id AND s.id = d.entity_id \
                     AND s.lifecycle = 'active'))) \
             ORDER BY bm25(search_document_fts), d.entity_id LIMIT ?",
        )
        .bind(fts_query)
        .bind(id_text(workspace_id))
        .bind(id_text(principal_id))
        .bind(Capability::KnowledgeRetrieve.metadata().mcp_name)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("lexical search failed"))?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let entity_id = row_id(&row, "entity_id")?;
            let kind = EntityKind::parse(&row_text(&row, "entity_kind")?)
                .ok_or_else(|| storage_error("invalid search entity kind"))?;
            let sources = self
                .load_search_sources(workspace_id, entity_id, kind)
                .await?;
            candidates.push(SearchCandidate {
                entity_id,
                kind,
                snippet: row_text(&row, "snippet")?,
                sources,
            });
        }
        Ok(candidates)
    }

    /// Stores one ready fixed-length little-endian `f32` embedding.
    ///
    /// # Errors
    /// Returns a validation error for inconsistent metadata or vector content,
    /// not-found if the indexed entity does not exist, or a redacted storage error.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_embedding(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        model_id: &str,
        model_version: &str,
        dimensions: usize,
        vector: &[u8],
    ) -> Result<(), ApplicationError> {
        let dimensions_i64 =
            i64::try_from(dimensions).map_err(|_| ApplicationError::Validation {
                field: "embedding_dimensions",
            })?;
        let embedding = Embedding::from_le_bytes(model_id, model_version, dimensions, vector)?;
        let result = sqlx::query(
            "INSERT INTO embedding (workspace_id, entity_id, model_id, model_version, \
             dimensions, content_hash, vector, index_state) \
             SELECT ?, ?, ?, ?, ?, d.content_hash, ?, 'ready' \
             FROM search_document AS d WHERE d.workspace_id = ? AND d.entity_id = ? \
             ON CONFLICT (workspace_id, entity_id, model_id, model_version) DO UPDATE SET \
             dimensions = excluded.dimensions, content_hash = excluded.content_hash, \
             vector = excluded.vector, index_state = 'ready', updated_at = CURRENT_TIMESTAMP",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .bind(embedding.model_id())
        .bind(embedding.model_version())
        .bind(dimensions_i64)
        .bind(embedding.to_le_bytes())
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("embedding upsert failed"))?;
        if result.rows_affected() == 0 {
            return Err(ApplicationError::NotFound { entity: "entity" });
        }
        Ok(())
    }

    /// Loads a bounded, deterministic set of authorized active vectors for one
    /// embedding model after requiring the ready vector's hash to match current text.
    ///
    /// # Errors
    /// Returns a redacted storage error for invalid persisted data or query failure.
    pub async fn embedding_search_candidates(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        model_id: &str,
        model_version: &str,
        max_records: std::num::NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError> {
        let rows = sqlx::query(
            "SELECT d.entity_id, d.entity_kind, d.snippet, e.model_id, e.model_version, \
             e.dimensions, e.vector FROM embedding AS e \
             JOIN search_document AS d ON d.workspace_id = e.workspace_id \
                 AND d.entity_id = e.entity_id \
             WHERE e.workspace_id = ? AND e.model_id = ? AND e.model_version = ? \
             AND e.index_state = 'ready' AND e.content_hash = d.content_hash \
             AND EXISTS (SELECT 1 FROM capability_grant AS grant_row \
                 WHERE grant_row.workspace_id = d.workspace_id \
                 AND grant_row.principal_id = ? AND grant_row.capability = ?) \
             AND ((d.entity_kind = 'note' AND EXISTS (SELECT 1 FROM note AS n \
                     WHERE n.workspace_id = d.workspace_id AND n.id = d.entity_id \
                     AND n.lifecycle = 'active')) \
                 OR (d.entity_kind = 'task' AND EXISTS (SELECT 1 FROM task AS t \
                     WHERE t.workspace_id = d.workspace_id AND t.id = d.entity_id \
                     AND t.lifecycle = 'active')) \
                 OR (d.entity_kind = 'memory' AND EXISTS (SELECT 1 FROM memory_assertion AS m \
                     WHERE m.workspace_id = d.workspace_id AND m.id = d.entity_id \
                     AND m.lifecycle = 'active' AND m.status = 'active') \
                     AND EXISTS (SELECT 1 FROM memory_source AS ms \
                         JOIN source AS provenance_source \
                           ON provenance_source.workspace_id = ms.workspace_id \
                          AND provenance_source.id = ms.source_id \
                         WHERE ms.workspace_id = d.workspace_id \
                           AND ms.memory_id = d.entity_id \
                           AND provenance_source.lifecycle = 'active')) \
                 OR (d.entity_kind = 'source' AND EXISTS (SELECT 1 FROM source AS s \
                     WHERE s.workspace_id = d.workspace_id AND s.id = d.entity_id \
                     AND s.lifecycle = 'active'))) \
             ORDER BY d.entity_id LIMIT ?",
        )
        .bind(id_text(workspace_id))
        .bind(model_id)
        .bind(model_version)
        .bind(id_text(principal_id))
        .bind(Capability::KnowledgeRetrieve.metadata().mcp_name)
        .bind(limit_i64(max_records))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("embedding search failed"))?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let entity_id = row_id(&row, "entity_id")?;
            let kind = EntityKind::parse(&row_text(&row, "entity_kind")?)
                .ok_or_else(|| storage_error("invalid search entity kind"))?;
            let dimensions: i64 = row
                .try_get("dimensions")
                .map_err(|_| storage_error("invalid embedding row"))?;
            let dimensions = usize::try_from(dimensions)
                .map_err(|_| storage_error("invalid embedding dimensions"))?;
            let vector: Vec<u8> = row
                .try_get("vector")
                .map_err(|_| storage_error("invalid embedding row"))?;
            let model_id = row_text(&row, "model_id")?;
            let model_version = row_text(&row, "model_version")?;
            let embedding = Embedding::from_le_bytes(model_id, model_version, dimensions, &vector)
                .map_err(|_| storage_error("invalid embedding row"))?;
            candidates.push(IndexedVector {
                candidate: SearchCandidate {
                    entity_id,
                    kind,
                    snippet: row_text(&row, "snippet")?,
                    sources: self
                        .load_search_sources(workspace_id, entity_id, kind)
                        .await?,
                },
                embedding,
            });
        }
        Ok(candidates)
    }

    async fn canonical_entity_exists(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        kind: EntityKind,
    ) -> Result<bool, ApplicationError> {
        let statement = match kind {
            EntityKind::Note => {
                "SELECT EXISTS(SELECT 1 FROM note WHERE workspace_id = ? AND id = ?)"
            }
            EntityKind::Task => {
                "SELECT EXISTS(SELECT 1 FROM task WHERE workspace_id = ? AND id = ?)"
            }
            EntityKind::Memory => {
                "SELECT EXISTS(SELECT 1 FROM memory_assertion WHERE workspace_id = ? AND id = ?)"
            }
            EntityKind::Source => {
                "SELECT EXISTS(SELECT 1 FROM source WHERE workspace_id = ? AND id = ?)"
            }
        };
        sqlx::query_scalar(statement)
            .bind(id_text(workspace_id))
            .bind(id_text(entity_id))
            .fetch_one(&self.pool)
            .await
            .map_err(|_| storage_error("search entity lookup failed"))
    }

    async fn load_search_sources(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
        kind: EntityKind,
    ) -> Result<Vec<SourceRef>, ApplicationError> {
        match kind {
            EntityKind::Memory => {
                let ids: Vec<String> = sqlx::query_scalar(
                    "SELECT ms.source_id FROM memory_source AS ms \
                     JOIN source AS s ON s.workspace_id = ms.workspace_id AND s.id = ms.source_id \
                     WHERE ms.workspace_id = ? AND ms.memory_id = ? \
                     AND s.lifecycle = 'active' ORDER BY ms.source_id",
                )
                .bind(id_text(workspace_id))
                .bind(id_text(entity_id))
                .fetch_all(&self.pool)
                .await
                .map_err(|_| storage_error("search provenance lookup failed"))?;
                ids.iter()
                    .map(|source_id| parse_id(source_id).map(|source_id| SourceRef { source_id }))
                    .collect()
            }
            EntityKind::Source => Ok(vec![SourceRef {
                source_id: entity_id,
            }]),
            EntityKind::Note | EntityKind::Task => Ok(Vec::new()),
        }
    }
}

impl SearchIndex for SqliteRepositories {
    async fn is_authorized(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
    ) -> Result<bool, ApplicationError> {
        self.has_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
            .await
    }

    async fn lexical_candidates(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &str,
        limit: std::num::NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError> {
        self.lexical_search_candidates(workspace_id, principal_id, query, limit)
            .await
    }

    async fn semantic_records(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &Embedding,
        max_records: std::num::NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError> {
        self.embedding_search_candidates(
            workspace_id,
            principal_id,
            query.model_id(),
            query.model_version(),
            max_records,
        )
        .await
    }
}

fn phrase_query(query: &str) -> Result<String, ApplicationError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ApplicationError::Validation { field: "query" });
    }
    Ok(format!("\"{}\"", query.replace('"', "\"\"")))
}

fn limit_i64(limit: std::num::NonZeroUsize) -> i64 {
    i64::try_from(limit.get()).unwrap_or(i64::MAX)
}

fn search_content_hash(snippet: &str) -> [u8; 32] {
    let digest = Sha256::digest(snippet.as_bytes());
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&digest);
    hash
}

impl NoteRepository for SqliteRepositories {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        entity_id: EntityId,
    ) -> Result<Option<Note>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, title, content, revision, lifecycle \
              FROM note WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("note lookup failed"))?;
        row.map(|row| decode_note(&row)).transpose()
    }

    async fn find_history(
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
        .map_err(|_| storage_error("note history lookup failed"))?;
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
             FROM task WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("task lookup failed"))?;
        row.map(|row| decode_task(&row)).transpose()
    }

    async fn find_history(
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
        .map_err(|_| storage_error("task history lookup failed"))?;
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
             FROM source WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'",
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
             FROM memory_assertion WHERE workspace_id = ? AND id = ? \
             AND lifecycle = 'active' AND status = 'active'",
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

    async fn find_history(
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
        .map_err(|_| storage_error("memory history lookup failed"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let sources = load_memory_sources(&self.pool, workspace_id, entity_id).await?;
        decode_memory(&row, sources).map(Some)
    }
}

async fn load_memory_sources(
    pool: &SqlitePool,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
) -> Result<Vec<SourceRef>, ApplicationError> {
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_id FROM memory_source \
         WHERE workspace_id = ? AND memory_id = ? ORDER BY source_id",
    )
    .bind(id_text(workspace_id))
    .bind(id_text(entity_id))
    .fetch_all(pool)
    .await
    .map_err(|_| storage_error("memory provenance lookup failed"))?;
    source_ids
        .iter()
        .map(|source_id| parse_id(source_id).map(|source_id| SourceRef { source_id }))
        .collect()
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

#[cfg(test)]
mod search_tests {
    use super::search_content_hash;

    #[test]
    fn canonical_search_hash_is_sha256_of_exact_utf8_text() {
        assert_eq!(
            search_content_hash("hello world"),
            [
                0xb9, 0x4d, 0x27, 0xb9, 0x93, 0x4d, 0x3e, 0x08, 0xa5, 0x2e, 0x52, 0xd7, 0xda, 0x7d,
                0xab, 0xfa, 0xc4, 0x84, 0xef, 0xe3, 0x7a, 0x53, 0x80, 0xee, 0x90, 0x88, 0xf7, 0xac,
                0xe2, 0xef, 0xcd, 0xe9,
            ]
        );
    }
}
