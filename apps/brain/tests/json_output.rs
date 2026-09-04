use brain::{CliEnvelope, render_json};
use serde_json::json;

#[test]
fn json_renderer_has_stable_result_envelope() {
    let rendered = render_json(&CliEnvelope::success(json!({"id":"019"}))).expect("renders");
    assert_eq!(
        rendered,
        "{\"ok\":true,\"data\":{\"id\":\"019\"},\"error\":null}\n"
    );
}

#[test]
fn json_renderer_has_stable_error_envelope() {
    let rendered =
        render_json(&CliEnvelope::<serde_json::Value>::error("unavailable")).expect("renders");
    assert_eq!(
        rendered,
        "{\"ok\":false,\"data\":null,\"error\":{\"code\":\"unavailable\"}}\n"
    );
}
