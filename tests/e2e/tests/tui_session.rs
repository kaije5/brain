use std::path::Path;

use brain::tui::{App, Tab};
use cortexd::{AuthenticatedIpcClient, DaemonRequest, PROTOCOL_VERSION};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use support::Harness;
use uuid::Uuid;

mod support;

fn render_app(app: &App) -> String {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| brain::tui::render(app, frame.area(), frame.buffer_mut()))
        .expect("draw succeeds");
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn owner_client(database_path: &Path) -> AuthenticatedIpcClient {
    AuthenticatedIpcClient::from_database_path(database_path).expect("owner enrollment")
}

fn request(capability: &str, payload: serde_json::Value) -> DaemonRequest {
    DaemonRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        operation_id: Uuid::now_v7(),
        capability: capability.to_owned(),
        payload,
    }
}

/// Drives the TUI state machine non-interactively against a real daemon:
/// a task write goes through the policy-checked IPC path, the tasks tab
/// renders the canonical state, and the chat tab degrades explicitly when
/// the fake model endpoint is stopped.
#[tokio::test]
async fn tui_renders_real_daemon_state_and_degrades_without_a_model() {
    let harness = Harness::start().await;
    let client = owner_client(harness.database_path());

    client
        .request(&request(
            "cortex_task_create",
            json!({"title": "Drive the TUI end to end"}),
        ))
        .await
        .expect("policy-checked task write through cortexd");
    let listed = client
        .request(&request("cortex_task_list", json!({"limit": 20})))
        .await
        .expect("task list through cortexd");

    let mut app = App::new();
    if let cortexd::WireResult::Success { value } = listed.result {
        let rows = value
            .as_array()
            .expect("task list array")
            .iter()
            .filter_map(|task| {
                Some((
                    task.get("title")?.as_str()?.to_owned(),
                    task.get("status")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        app.set_tasks(rows);
    }
    app.select_tab(Tab::Tasks);
    let screen = render_app(&app);
    assert!(
        screen.contains("Drive the TUI end to end"),
        "tasks tab must render canonical daemon state, got:\n{screen}"
    );

    // Stopping the fake model endpoint leaves deterministic tabs working
    // while the chat tab reports the explicit degraded state.
    harness.stop_fake_model().await;
    app.select_tab(Tab::Chat);
    for character in "hello".chars() {
        app.push_chat_input(character);
    }
    app.submit_prompt();
    let response = client
        .request(&request("cortex_agent_run", json!({"prompt": "hello"})))
        .await
        .expect("agent run reaches the daemon");
    match response.result {
        cortexd::WireResult::Error { code } => app.receive_agent_error(code),
        cortexd::WireResult::Success { .. } => {
            app.receive_agent_reply("unexpected success".to_owned());
        }
    }
    let screen = render_app(&app);
    assert!(
        screen.to_lowercase().contains("degraded"),
        "chat tab must show the explicit degraded state, got:\n{screen}"
    );
}
