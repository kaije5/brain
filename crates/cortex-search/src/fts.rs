use std::num::NonZeroUsize;

use cortex_application::{ApplicationError, Capability};
use cortex_domain::{EntityId, PrincipalId, SourceRef, WorkspaceId};
use cortex_storage::{SearchEntityKind, SqliteRepositories};

use crate::Embedding;

pub use cortex_storage::SearchEntityKind as EntityKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCandidate {
    pub entity_id: EntityId,
    pub kind: SearchEntityKind,
    pub snippet: String,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexedVector {
    pub candidate: SearchCandidate,
    pub embedding: Embedding,
}

#[allow(async_fn_in_trait)]
pub trait SearchIndex: Send + Sync {
    async fn is_authorized(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
    ) -> Result<bool, ApplicationError>;

    async fn lexical_candidates(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &str,
        limit: NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError>;

    async fn semantic_records(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &Embedding,
        max_records: NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError>;
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
        limit: NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError> {
        self.lexical_search_candidates(workspace_id, principal_id, query, limit)
            .await
            .map(|candidates| {
                candidates
                    .into_iter()
                    .map(|candidate| SearchCandidate {
                        entity_id: candidate.entity_id,
                        kind: candidate.kind,
                        snippet: candidate.snippet,
                        sources: candidate.sources,
                    })
                    .collect()
            })
    }

    async fn semantic_records(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &Embedding,
        max_records: NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError> {
        self.embedding_search_candidates(
            workspace_id,
            principal_id,
            query.model_id(),
            query.model_version(),
            max_records,
        )
        .await?
        .into_iter()
        .map(|record| {
            Ok(IndexedVector {
                candidate: SearchCandidate {
                    entity_id: record.candidate.entity_id,
                    kind: record.candidate.kind,
                    snippet: record.candidate.snippet,
                    sources: record.candidate.sources,
                },
                embedding: Embedding::from_le_bytes(
                    record.model_id,
                    record.model_version,
                    record.dimensions,
                    &record.vector,
                )?,
            })
        })
        .collect()
    }
}
