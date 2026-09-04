use brain::{Cli, command_request};
use clap::Parser;
use serde_json::json;

#[test]
fn note_create_maps_only_client_safe_payload_fields() {
    let cli = Cli::try_parse_from(["brain", "note", "create", "Title", "Body"]).expect("parses");
    let request = command_request(&cli).expect("maps");
    assert_eq!(request.capability, "cortex_note_create");
    assert_eq!(request.payload, json!({"title":"Title","content":"Body"}));
    assert_ne!(request.request_id, request.operation_id);
}

#[test]
fn ask_maps_to_daemon_agent_without_model_configuration() {
    let cli = Cli::try_parse_from(["brain", "ask", "What is next?"]).expect("parses");
    let request = command_request(&cli).expect("maps");
    assert_eq!(request.capability, "cortex_agent_run");
    assert_eq!(request.payload, json!({"prompt":"What is next?"}));
}

#[test]
fn task_due_date_is_normalized_to_daemon_rfc3339() {
    let cli = Cli::try_parse_from(["brain", "task", "add", "Finish", "--due", "2026-09-01"])
        .expect("parses");
    let request = command_request(&cli).expect("maps");
    assert_eq!(
        request.payload,
        json!({"title":"Finish","due_at":"2026-09-01T00:00:00+00:00"})
    );
}
