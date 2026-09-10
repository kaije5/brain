use std::sync::{Arc, Mutex};

use brain::tui::{App, Tab};
use brain::{CommandRequest, DaemonClient};
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

/// SCRUM-73: a streaming `cortex_agent_run` delivers ordered partial frames
/// on the authenticated local IPC session before the terminal frame, and the
/// TUI chat tab renders the partial assistant output while the run is still
/// in flight.
#[tokio::test]
async fn agent_run_streams_frames_and_the_tui_renders_them_incrementally() {
    let harness = Harness::start().await;
    let client =
        DaemonClient::from_database_path(harness.database_path()).expect("enrolled client");
    let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = chunks.clone();
    let on_partial = move |chunk: &str| {
        recorded.lock().expect("chunk log").push(chunk.to_owned());
    };
    let response = client
        .request_streaming(
            CommandRequest {
                request_id: Uuid::now_v7(),
                operation_id: Uuid::now_v7(),
                capability: "cortex_agent_run".to_owned(),
                payload: json!({"prompt": "hello"}),
            },
            &on_partial,
        )
        .await
        .expect("streaming agent run reaches the daemon");

    let final_content = match response.result {
        cortexd::WireResult::Success { value } => value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .expect("terminal content")
            .to_owned(),
        cortexd::WireResult::Error { code } => panic!("streaming run failed: {code}"),
    };
    let streamed = chunks.lock().expect("chunk log").clone();
    assert!(
        !streamed.is_empty(),
        "at least one partial frame must arrive before the terminal frame"
    );
    assert!(streamed.iter().all(|chunk| !chunk.trim().is_empty()));

    let mut app = App::new();
    app.select_tab(Tab::Chat);
    for character in "hello".chars() {
        app.push_chat_input(character);
    }
    app.submit_prompt();
    for chunk in &streamed {
        app.receive_agent_partial(chunk);
    }
    let partial_screen = render_app(&app);
    assert!(
        partial_screen.contains(streamed[0].as_str()),
        "chat tab must render the streamed partial reply, got:\n{partial_screen}"
    );
    assert!(app.partial_reply().is_some());

    app.receive_agent_reply(final_content.clone());
    let final_screen = render_app(&app);
    assert!(
        final_screen.contains(final_content.as_str()),
        "chat tab must render the finalized reply, got:\n{final_screen}"
    );
    assert!(app.partial_reply().is_none());
}
