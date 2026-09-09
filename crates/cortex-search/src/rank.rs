use std::collections::BTreeMap;

use cortex_domain::EntityId;

#[derive(Clone, Debug, PartialEq)]
pub struct RankedEntity {
    pub entity_id: EntityId,
    pub lexical_rank: Option<u32>,
    pub semantic_rank: Option<u32>,
    pub fused_score: f64,
}

#[must_use]
pub fn reciprocal_rank_fusion(
    lexical: Vec<EntityId>,
    semantic: Vec<EntityId>,
    k: u32,
) -> Vec<RankedEntity> {
    let mut ranks = BTreeMap::<EntityId, RankedEntity>::new();
    add_ranked_leg(&mut ranks, lexical, k, true);
    add_ranked_leg(&mut ranks, semantic, k, false);

    let mut ranked = ranks.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
    ranked
}

fn add_ranked_leg(
    fused: &mut BTreeMap<EntityId, RankedEntity>,
    entities: Vec<EntityId>,
    k: u32,
    lexical: bool,
) {
    for (index, entity_id) in entities.into_iter().enumerate() {
        let rank = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let hit = fused.entry(entity_id).or_insert(RankedEntity {
            entity_id,
            lexical_rank: None,
            semantic_rank: None,
            fused_score: 0.0,
        });
        hit.fused_score += 1.0 / (f64::from(k) + f64::from(rank));
        if lexical {
            hit.lexical_rank = Some(rank);
        } else {
            hit.semantic_rank = Some(rank);
        }
    }
}
