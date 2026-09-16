//! Fusion of provenance-bearing vault chunks with Cortex-owned AI memories
//! (SCRUM-130).
//!
//! Vault knowledge retrieval ([`crate::vault_retrieval`]) and memory search
//! ([`crate::service`]) are separate legs over separate authorities: the
//! vault is authoritative for user-authored knowledge, Cortex for AI
//! memories. [`fuse_with_memories`] combines both legs with reciprocal-rank
//! fusion into one deterministic, provenance-preserving result list.

use std::num::NonZeroUsize;

use crate::{VaultHit, service::SearchHit};

const RRF_K: u32 = 60;

/// One fused retrieval result: either a vault chunk (with its chunk
/// reference and snippet) or a Cortex-owned memory hit.
#[derive(Clone, Debug, PartialEq)]
pub enum FusedLeg {
    VaultChunk(VaultHit),
    Memory(SearchHit),
}

/// A fused hit with its per-leg ranks and fused score.
#[derive(Clone, Debug, PartialEq)]
pub struct FusedHit {
    pub leg: FusedLeg,
    pub fused_score: f64,
}

/// Stable ordering key used as the deterministic score tiebreak.
fn tiebreak(hit: &FusedHit) -> Vec<u8> {
    match &hit.leg {
        FusedLeg::VaultChunk(vault_hit) => {
            let mut key = vec![0_u8];
            key.extend(vault_hit.reference.resource.provider_id().as_str().bytes());
            key.push(0);
            key.extend(vault_hit.reference.resource.resource_id().as_str().bytes());
            key.push(0);
            key.extend(vault_hit.reference.chunk.get().to_be_bytes());
            key
        }
        FusedLeg::Memory(memory_hit) => {
            let mut key = vec![1_u8];
            key.extend(uuid::Uuid::from(memory_hit.entity_id).as_bytes());
            key
        }
    }
}

/// Fuses vault retrieval results (which already fuse their own lexical and
/// semantic sub-legs and carry ranks on each hit) with memory search hits
/// (ranked by their own fused score). Deterministic: score ties break by
/// vault chunk reference before memory entity id.
#[must_use]
pub fn fuse_with_memories(
    vault: crate::vault_retrieval::RetrievalOutcome,
    memories: Vec<SearchHit>,
    limit: NonZeroUsize,
) -> Vec<FusedHit> {
    let mut fused: Vec<FusedHit> = Vec::new();
    for (index, hit) in vault.hits.into_iter().enumerate() {
        let rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
        fused.push(FusedHit {
            leg: FusedLeg::VaultChunk(hit),
            fused_score: 1.0 / (f64::from(RRF_K) + f64::from(rank)),
        });
    }
    let mut ranked_memories = memories;
    ranked_memories.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
    for (index, hit) in ranked_memories.into_iter().enumerate() {
        let rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
        fused.push(FusedHit {
            leg: FusedLeg::Memory(hit),
            fused_score: 1.0 / (f64::from(RRF_K) + f64::from(rank)),
        });
    }
    fused.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| tiebreak(left).cmp(&tiebreak(right)))
    });
    fused.truncate(limit.get());
    fused
}
