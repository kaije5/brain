use cortex_application::{ApplicationError, Embedding};

#[test]
fn embedding_contract_rejects_non_finite_and_zero_norm_vectors() {
    for values in [vec![f32::NAN], vec![f32::INFINITY], vec![0.0, -0.0]] {
        assert_eq!(
            Embedding::new("nomic", "1", values),
            Err(ApplicationError::Validation {
                field: "embedding_vector"
            })
        );
    }
}

#[test]
fn embedding_contract_round_trips_exact_little_endian_payload() -> Result<(), String> {
    let embedding = Embedding::from_le_bytes("nomic", "1", 2, &[0, 0, 128, 63, 0, 0, 32, 192])
        .map_err(debug_error)?;

    assert_eq!(embedding.values(), &[1.0, -2.5]);
    assert_eq!(embedding.to_le_bytes(), [0, 0, 128, 63, 0, 0, 32, 192]);
    Ok(())
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
