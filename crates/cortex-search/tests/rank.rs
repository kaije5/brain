use cortex_domain::EntityId;
use cortex_search::reciprocal_rank_fusion;
use uuid::Uuid;

#[test]
fn reciprocal_rank_fusion_is_deterministic() -> Result<(), String> {
    let a = id("01900000-0000-7000-8000-000000000001")?;
    let b = id("01900000-0000-7000-8000-000000000002")?;

    let hits = reciprocal_rank_fusion(vec![a, b], vec![b, a], 60);

    assert_eq!(
        hits.into_iter()
            .map(|hit| hit.entity_id)
            .collect::<Vec<_>>(),
        vec![a, b]
    );
    Ok(())
}

fn id(value: &str) -> Result<EntityId, String> {
    let uuid = Uuid::parse_str(value).map_err(|error| error.to_string())?;
    EntityId::try_from(uuid).map_err(|error| format!("{error:?}"))
}
