use std::{num::NonZeroUsize, time::Duration};

use cortex_application::{
    ApplicationError, Embedding, EmbeddingProvider, EntityKind, IndexedVector, SearchCandidate,
    SearchIndex,
};
use cortex_domain::{EntityId, PrincipalId, SourceRef, WorkspaceId};
use cortex_search::{HybridSearchService, SearchRequest};

#[tokio::test]
async fn unavailable_embeddings_return_lexical_results_with_degraded_flag() -> Result<(), String> {
    let lexical = candidate("Cortex uses Nemotron as its local AI.");
    let service = HybridSearchService::new(FakeIndex::authorized(vec![lexical]), Unavailable);

    let hits = service
        .search(search_request("Nemotron")?)
        .await
        .map_err(debug_error)?;

    assert_eq!(hits[0].snippet, "Cortex uses Nemotron as its local AI.");
    assert_eq!(hits[0].lexical_rank, Some(1));
    assert_eq!(hits[0].semantic_rank, None);
    assert!(hits[0].semantic_degraded);
    Ok(())
}

#[tokio::test]
async fn semantic_timeout_returns_lexical_results_with_degraded_flag() -> Result<(), String> {
    let lexical = candidate("Cortex uses Nemotron as its local AI.");
    let service = HybridSearchService::new(FakeIndex::authorized(vec![lexical]), Slow).with_limits(
        NonZeroUsize::new(10).ok_or("non-zero scan bound required")?,
        Duration::from_millis(1),
    );

    let hits = service
        .search(search_request("Nemotron")?)
        .await
        .map_err(debug_error)?;

    assert_eq!(hits.len(), 1);
    assert!(hits[0].semantic_degraded);
    Ok(())
}

#[tokio::test]
async fn available_embeddings_add_semantic_candidates_and_fuse_ranks() -> Result<(), String> {
    let lexical = candidate("Cortex uses Nemotron as its local AI.");
    let semantic_only = SearchCandidate {
        entity_id: EntityId::new(),
        kind: EntityKind::Note,
        snippet: "Nemotron runs locally.".to_owned(),
        sources: Vec::new(),
    };
    let index = FakeIndex {
        authorized: true,
        lexical: vec![lexical.clone()],
        semantic: vec![
            indexed(semantic_only.clone(), vec![1.0, 0.0])?,
            indexed(lexical.clone(), vec![0.8, 0.2])?,
        ],
    };
    let service = HybridSearchService::new(index, Fixed(embedding(vec![1.0, 0.0])?));

    let hits = service
        .search(search_request("Nemotron")?)
        .await
        .map_err(debug_error)?;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].entity_id, lexical.entity_id);
    assert_eq!(hits[0].lexical_rank, Some(1));
    assert_eq!(hits[0].semantic_rank, Some(2));
    assert_eq!(hits[1].entity_id, semantic_only.entity_id);
    assert_eq!(hits[1].lexical_rank, None);
    assert_eq!(hits[1].semantic_rank, Some(1));
    assert!(hits.iter().all(|hit| !hit.semantic_degraded));
    Ok(())
}

#[tokio::test]
async fn missing_knowledge_capability_is_denied_before_search() -> Result<(), String> {
    let service = HybridSearchService::new(FakeIndex::denied(), FailsIfCalled);

    let result = service.search(search_request("Nemotron")?).await;

    assert_eq!(result, Err(ApplicationError::PermissionDenied));
    Ok(())
}

#[derive(Clone)]
struct FakeIndex {
    authorized: bool,
    lexical: Vec<SearchCandidate>,
    semantic: Vec<IndexedVector>,
}

impl FakeIndex {
    fn authorized(lexical: Vec<SearchCandidate>) -> Self {
        Self {
            authorized: true,
            lexical,
            semantic: Vec::new(),
        }
    }

    fn denied() -> Self {
        Self {
            authorized: false,
            lexical: Vec::new(),
            semantic: Vec::new(),
        }
    }
}

impl SearchIndex for FakeIndex {
    async fn is_authorized(
        &self,
        _workspace_id: WorkspaceId,
        _principal_id: PrincipalId,
    ) -> Result<bool, ApplicationError> {
        Ok(self.authorized)
    }

    async fn lexical_candidates(
        &self,
        _workspace_id: WorkspaceId,
        _principal_id: PrincipalId,
        _query: &str,
        _limit: NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError> {
        Ok(self.lexical.clone())
    }

    async fn semantic_records(
        &self,
        _workspace_id: WorkspaceId,
        _principal_id: PrincipalId,
        _query: &Embedding,
        max_records: NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError> {
        Ok(self
            .semantic
            .iter()
            .take(max_records.get())
            .cloned()
            .collect())
    }
}

struct Unavailable;

impl EmbeddingProvider for Unavailable {
    async fn embed(&self, _text: &str) -> Result<Embedding, ApplicationError> {
        Err(ApplicationError::InferenceUnavailable)
    }
}

struct Slow;

impl EmbeddingProvider for Slow {
    async fn embed(&self, _text: &str) -> Result<Embedding, ApplicationError> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        embedding(vec![1.0, 0.0]).map_err(|_| ApplicationError::Internal)
    }
}

struct Fixed(Embedding);

impl EmbeddingProvider for Fixed {
    async fn embed(&self, _text: &str) -> Result<Embedding, ApplicationError> {
        Ok(self.0.clone())
    }
}

struct FailsIfCalled;

impl EmbeddingProvider for FailsIfCalled {
    async fn embed(&self, _text: &str) -> Result<Embedding, ApplicationError> {
        Err(ApplicationError::Internal)
    }
}

fn search_request(query: &str) -> Result<SearchRequest, String> {
    Ok(SearchRequest {
        workspace_id: WorkspaceId::new(),
        principal_id: PrincipalId::new(),
        query: query.to_owned(),
        limit: NonZeroUsize::new(10).ok_or("non-zero result limit required")?,
    })
}

fn candidate(snippet: &str) -> SearchCandidate {
    SearchCandidate {
        entity_id: EntityId::new(),
        kind: EntityKind::Memory,
        snippet: snippet.to_owned(),
        sources: vec![SourceRef {
            source_id: EntityId::new(),
        }],
    }
}

fn indexed(candidate: SearchCandidate, values: Vec<f32>) -> Result<IndexedVector, String> {
    Ok(IndexedVector {
        candidate,
        embedding: embedding(values)?,
    })
}

fn embedding(values: Vec<f32>) -> Result<Embedding, String> {
    Embedding::new("nomic", "1", values).map_err(debug_error)
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
