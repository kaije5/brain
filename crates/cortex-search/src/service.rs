use std::{collections::BTreeMap, num::NonZeroUsize, time::Duration};

use cortex_application::ApplicationError;
use cortex_domain::{EntityId, PrincipalId, SourceRef, WorkspaceId};

use crate::{
    EmbeddingProvider, EntityKind, SearchCandidate, SearchIndex, VectorRecord, cosine_candidates,
    reciprocal_rank_fusion,
};

const RRF_K: u32 = 60;
const DEFAULT_MAX_INDEXED_RECORDS: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    pub workspace_id: WorkspaceId,
    pub principal_id: PrincipalId,
    pub query: String,
    pub limit: NonZeroUsize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub entity_id: EntityId,
    pub kind: EntityKind,
    pub snippet: String,
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
    pub fused_score: f64,
    pub sources: Vec<SourceRef>,
    pub semantic_degraded: bool,
}

pub struct HybridSearchService<I, E> {
    index: I,
    embeddings: E,
    max_indexed_records: NonZeroUsize,
    query_timeout: Duration,
}

impl<I, E> HybridSearchService<I, E> {
    #[must_use]
    pub fn new(index: I, embeddings: E) -> Self {
        Self {
            index,
            embeddings,
            max_indexed_records: NonZeroUsize::new(DEFAULT_MAX_INDEXED_RECORDS)
                .unwrap_or(NonZeroUsize::MIN),
            query_timeout: Duration::from_secs(2),
        }
    }

    #[must_use]
    pub const fn with_limits(
        mut self,
        max_indexed_records: NonZeroUsize,
        query_timeout: Duration,
    ) -> Self {
        self.max_indexed_records = max_indexed_records;
        self.query_timeout = query_timeout;
        self
    }
}

impl<I, E> HybridSearchService<I, E>
where
    I: SearchIndex,
    E: EmbeddingProvider,
{
    /// Performs authorized lexical retrieval and an optional bounded semantic leg.
    ///
    /// # Errors
    /// Returns typed validation, authorization, storage, or embedding errors.
    /// Provider unavailability and semantic timeouts degrade to lexical results.
    pub async fn search(&self, request: SearchRequest) -> Result<Vec<SearchHit>, ApplicationError> {
        if request.query.trim().is_empty() {
            return Err(ApplicationError::Validation { field: "query" });
        }
        if !self
            .index
            .is_authorized(request.workspace_id, request.principal_id)
            .await?
        {
            return Err(ApplicationError::PermissionDenied);
        }
        let lexical = self
            .index
            .lexical_candidates(
                request.workspace_id,
                request.principal_id,
                &request.query,
                request.limit,
            )
            .await?;
        let semantic = tokio::time::timeout(self.query_timeout, async {
            let query_embedding = self.embeddings.embed(&request.query).await?;
            let records = self
                .index
                .semantic_records(
                    request.workspace_id,
                    request.principal_id,
                    &query_embedding,
                    self.max_indexed_records,
                )
                .await?;
            let ranked = cosine_candidates(
                &query_embedding,
                records
                    .iter()
                    .map(|record| {
                        VectorRecord::new(record.candidate.entity_id, record.embedding.clone())
                    })
                    .collect(),
                self.max_indexed_records,
            )?;
            Ok::<_, ApplicationError>((records, ranked))
        })
        .await;

        let (semantic_records, semantic_ranked, semantic_degraded) = match semantic {
            Ok(Ok((records, ranked))) => (records, ranked, false),
            // Transient embedding failures degrade to lexical-only results;
            // permanent configuration failures still surface as typed errors.
            Ok(Err(error))
                if error.recovery_hint()
                    == cortex_application::RecoveryHint::RetrySameSelection =>
            {
                (Vec::new(), Vec::new(), true)
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => (Vec::new(), Vec::new(), true),
        };

        let mut details = BTreeMap::<EntityId, SearchCandidate>::new();
        for candidate in &lexical {
            details.insert(candidate.entity_id, candidate.clone());
        }
        for record in semantic_records {
            details
                .entry(record.candidate.entity_id)
                .or_insert(record.candidate);
        }
        let fused = reciprocal_rank_fusion(
            lexical
                .iter()
                .map(|candidate| candidate.entity_id)
                .collect(),
            semantic_ranked
                .iter()
                .map(|candidate| candidate.entity_id)
                .collect(),
            RRF_K,
        );

        fused
            .into_iter()
            .take(request.limit.get())
            .map(|ranked| {
                let candidate = details
                    .remove(&ranked.entity_id)
                    .ok_or(ApplicationError::Internal)?;
                Ok(SearchHit {
                    entity_id: ranked.entity_id,
                    kind: candidate.kind,
                    snippet: candidate.snippet,
                    lexical_rank: ranked.lexical_rank,
                    semantic_rank: ranked.semantic_rank,
                    fused_score: ranked.fused_score,
                    sources: candidate.sources,
                    semantic_degraded,
                })
            })
            .collect()
    }
}
