//! Extraction of Markdown content into the derived index, and deterministic
//! lexical/semantic/hybrid retrieval over it (SCRUM-112).
//!
//! Pure layer: parsing goes through `cortex-vault`, chunking through
//! [`crate::chunk_body`], storage through [`DerivedVaultIndex`]. Retrieval
//! returns stable provider/chunk-backed hits with bounded snippets; the
//! semantic leg degrades explicitly rather than silently disappearing.

use std::num::NonZeroUsize;

use crate::{
    ChunkProvenance, ChunkReference, DerivedVaultIndex, DocumentIndexEntry, IndexedChunk,
    chunk_body,
};
use cortex_domain::{ProviderResourceKind, ProviderResourceRef};

const RRF_K: u32 = 60;
const MAX_SNIPPET_CHARS: usize = 240;

/// A provider/chunk-backed retrieval hit with a bounded snippet.
#[derive(Clone, Debug, PartialEq)]
pub struct VaultHit {
    pub reference: ChunkReference,
    pub snippet: String,
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
    pub fused_score: f64,
}

/// A retrieval outcome: hits plus whether the semantic leg was available.
/// `semantic_degraded == true` means only lexical results are present.
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalOutcome {
    pub hits: Vec<VaultHit>,
    pub semantic_degraded: bool,
}

/// Extracts one Markdown document into the derived index: document-level
/// metadata (title, headings, tags, links, task metadata) plus provenance-
/// stamped body chunks. Returns the number of chunks indexed.
///
/// # Errors
/// Returns a redacted [`cortex_vault::VaultFormatError`] when the text is
/// not valid supported Markdown (encoding, malformed frontmatter, bounds).
pub fn index_document(
    index: &mut DerivedVaultIndex,
    resource: &ProviderResourceRef,
    text: &str,
    provenance: &ChunkProvenance,
) -> Result<usize, cortex_vault::VaultFormatError> {
    let parsed = cortex_vault::parse_document(text)?;
    let frontmatter = parsed.frontmatter();

    // Task metadata only for task-kind resources (format spec §6).
    let (task_status, task_priority) = if resource.kind() == ProviderResourceKind::Task {
        match frontmatter {
            Some(block) => (
                block.scalar("status").map(str::to_owned),
                block.scalar("priority").map(str::to_owned),
            ),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    // Replace prior chunks and the document entry for this resource with the
    // freshly extracted state.
    index.remove_resource(resource);

    index
        .upsert_document(DocumentIndexEntry {
            resource: resource.clone(),
            title: parsed.title("").into_owned(),
            tags: parsed.tags().to_vec(),
            headings: parsed.headings().to_vec(),
            links: parsed
                .wikilinks()
                .iter()
                .map(|link| link.target.clone())
                .collect(),
            task_status,
            task_priority,
        })
        .map_err(|_| cortex_vault::VaultFormatError::TooLarge)?;

    let mut indexed = 0_usize;
    for (ordinal, chunk_text) in chunk_body(parsed.body()) {
        let reference = ChunkReference::new(resource.clone(), ordinal);
        let chunk = IndexedChunk::new(reference, chunk_text, provenance.clone())
            .map_err(|_| cortex_vault::VaultFormatError::TooLarge)?;
        index
            .upsert_chunk(chunk)
            .map_err(|_| cortex_vault::VaultFormatError::TooLarge)?;
        indexed += 1;
    }
    Ok(indexed)
}

/// Case-insensitive containment retrieval across chunk text, document
/// titles, and tags. Deterministic: index order, ties by chunk reference.
///
/// # Errors
/// Returns a redacted validation error when the query is blank.
pub fn lexical(
    index: &DerivedVaultIndex,
    query: &str,
    limit: NonZeroUsize,
) -> Result<Vec<VaultHit>, cortex_domain::DomainError> {
    if query.trim().is_empty() {
        return Err(cortex_domain::DomainError::validation(
            "query",
            "query must not be blank",
        ));
    }
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for (reference, chunk) in index.chunks() {
        // Metadata (title/tags) matches when a document entry is present.
        let metadata_match = index.document(&reference.resource).is_some_and(|entry| {
            entry.title.to_lowercase().contains(&needle)
                || entry
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle))
        });
        let text_match = chunk.text.to_lowercase().contains(&needle);
        if !metadata_match && !text_match {
            continue;
        }
        if hits.len() >= limit.get() {
            return Ok(hits);
        }
        hits.push(VaultHit {
            reference: reference.clone(),
            snippet: snippet(&chunk.text),
            lexical_rank: None,
            semantic_rank: None,
            fused_score: 0.0,
        });
    }
    Ok(hits)
}

/// Cosine retrieval over chunks that carry embeddings. Chunks without an
/// embedding are invisible to this leg — degradation is decided by callers.
///
/// # Errors
/// Returns a redacted validation error when the query embedding is blank or
/// a stored embedding is incompatible.
pub fn semantic(
    index: &DerivedVaultIndex,
    query: &crate::Embedding,
    limit: NonZeroUsize,
) -> Result<Vec<VaultHit>, cortex_domain::DomainError> {
    let mut scored: Vec<(f64, &ChunkReference, &str)> = Vec::new();
    for (reference, indexed) in index.chunks() {
        let Some(embedding) = &indexed.embedding else {
            continue;
        };
        let similarity = cosine(query.values(), embedding.values())?;
        scored.push((similarity, reference, indexed.text.as_str()));
    }
    scored.sort_by(|left, right| right.0.total_cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    let mut hits = Vec::new();
    for (_, reference, text) in scored.into_iter().take(limit.get()) {
        hits.push(VaultHit {
            reference: reference.clone(),
            snippet: snippet(text),
            lexical_rank: None,
            semantic_rank: None,
            fused_score: 0.0,
        });
    }
    Ok(hits)
}

/// Hybrid retrieval: reciprocal-rank fusion of the lexical and semantic legs.
/// When `query_embedding` is `None` (embeddings unavailable) the outcome is
/// explicitly marked degraded and only lexical results are returned.
///
/// # Errors
/// Returns a redacted validation error when the query is blank or an
/// embedding is incompatible.
pub fn hybrid(
    index: &DerivedVaultIndex,
    query: &str,
    query_embedding: Option<&crate::Embedding>,
    limit: NonZeroUsize,
) -> Result<RetrievalOutcome, cortex_domain::DomainError> {
    let lexical_hits = lexical(index, query, limit)?;
    let (semantic_hits, degraded) = match query_embedding {
        Some(embedding) => (semantic(index, embedding, limit)?, false),
        None => (Vec::new(), true),
    };

    // Fuse on chunk references: score += 1/(k + rank) per leg.
    let mut fused: Vec<VaultHit> = Vec::new();
    let fuse_leg = |hits: Vec<VaultHit>, lexical: bool, fused: &mut Vec<VaultHit>| {
        for (index, hit) in hits.into_iter().enumerate() {
            let rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let score = 1.0 / (f64::from(RRF_K) + f64::from(rank));
            if let Some(existing) = fused
                .iter_mut()
                .find(|existing| existing.reference == hit.reference)
            {
                existing.fused_score += score;
                if lexical {
                    existing.lexical_rank = Some(rank);
                } else {
                    existing.semantic_rank = Some(rank);
                }
            } else {
                let mut hit = hit;
                hit.fused_score = score;
                if lexical {
                    hit.lexical_rank = Some(rank);
                    hit.semantic_rank = None;
                } else {
                    hit.semantic_rank = Some(rank);
                    hit.lexical_rank = None;
                }
                fused.push(hit);
            }
        }
    };
    fuse_leg(lexical_hits, true, &mut fused);
    fuse_leg(semantic_hits, false, &mut fused);

    fused.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| left.reference.cmp(&right.reference))
    });
    let hits = fused.into_iter().take(limit.get()).collect();
    Ok(RetrievalOutcome {
        hits,
        semantic_degraded: degraded,
    })
}

fn snippet(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() > MAX_SNIPPET_CHARS {
        first_line.chars().take(MAX_SNIPPET_CHARS).collect()
    } else {
        first_line.to_owned()
    }
}

fn cosine(left: &[f32], right: &[f32]) -> Result<f64, cortex_domain::DomainError> {
    if left.len() != right.len() {
        return Err(cortex_domain::DomainError::validation(
            "embedding_dimensions",
            "embedding dimensions must match",
        ));
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (l, r) in left.iter().zip(right.iter()) {
        let (l, r) = (f64::from(*l), f64::from(*r));
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return Err(cortex_domain::DomainError::validation(
            "embedding_vector",
            "embedding vectors must be non-zero",
        ));
    }
    Ok(dot / (left_norm.sqrt() * right_norm.sqrt()))
}
