//! Mid-session model switching and single-writer rendering (SCRUM-83).


use brain::tui::{App, InputCommand};
use brain::{Cli, command_request};
use clap::Parser;

fn render_app(app: &App) -> String {
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
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

#[test]
fn model_command_shapes_are_parsed_without_entering_waiting() {
    let mut app = App::new();

    // Bare `/model` becomes a catalog query, not an agent prompt.
    for c in "/model".chars() {
        app.push_chat_input(c);
    }
    let command = app.submit_input();
    assert!(matches!(command, Some(InputCommand::ModelList)));
    assert!(
        !matches!(app.chat_status(), brain::tui::ChatStatus::Waiting),
        "a model query must not enter the in-flight state"
    );

    // `/model <id>` becomes a selection for subsequent turns.
    for c in "/model other-model".chars() {
        app.push_chat_input(c);
    }
    let command = app.submit_input();
    assert!(matches!(
        command,
        Some(InputCommand::ModelSelect(model)) if model == "other-model"
    ));

    // A plain prompt still enters the in-flight state.
    for c in "hello".chars() {
        app.push_chat_input(c);
    }
    let command = app.submit_input();
    assert!(matches!(command, Some(InputCommand::Agent(prompt)) if prompt == "hello"));
    assert!(matches!(app.chat_status(), brain::tui::ChatStatus::Waiting));
}

#[tokio::test]
async fn in_flight_generation_rejects_a_model_switch_and_keeps_history() {
    let mut app = App::new();
    for c in "in-flight prompt".chars() {
        app.push_chat_input(c);
    }
    app.submit_prompt();
    app.receive_agent_partial("partial");
    assert!(matches!(app.chat_status(), brain::tui::ChatStatus::Waiting));

    // While a generation is in flight, a selection attempt is rejected
    // instead of silently switching mid-stream.
    assert_eq!(app.submit_input(), None);

    // History is preserved and the partial reply untouched.
    assert!(app.pending_prompt().is_some());
    assert_eq!(app.partial_reply(), Some("partial"));
}

#[test]
fn single_writer_event_order_is_deterministic_under_rapid_events() {
    let mut app = App::new();
    // Rapid interleaved events from "concurrent" producers: partial deltas,
    // status changes, more deltas. Everything flows through the single
    // App mutation path in event order.
    app.receive_agent_partial("tok");
    app.receive_agent_partial("en-1 ");
    app.receive_agent_partial("tok");
    app.receive_agent_partial("en-2");
    app.receive_agent_reply("token-1 token-2".to_owned());
    app.receive_agent_partial("final delta");
    app.receive_agent_reply("final".to_owned());

    let screen = render_app(&app);
    assert!(
        screen.contains("token-1 token-2"),
        "first reply rendered: {screen}"
    );
    assert!(screen.contains("final"), "second reply rendered: {screen}");
    assert!(
        !screen.contains("final deltatok"),
        "no corrupted interleaving: {screen}"
    );
}

#[test]
fn ui_status_state_never_enters_the_provider_request_payload() {
    // UI-only state is staged on the App for rendering...
    let mut app = App::new();
    app.set_active_model(Some("nim".to_owned()), vec!["nim".to_owned()]);
    app.set_status_line("streaming… 42 tokens".to_owned());
    app.apply_model_switch("other-model".to_owned());
    for c in "/model other-model".chars() {
        app.push_chat_input(c);
    }

    // ...but the wire payload for a prompt is built solely from the actual
    // user context.
    let cli = Cli::try_parse_from(["brain", "ask", "real user question"]).expect("parses");
    let request = command_request(&cli).expect("maps");
    let payload_text = request.payload.to_string();
    assert!(!payload_text.contains("streaming"), "{payload_text}");
    assert!(!payload_text.contains("42 tokens"), "{payload_text}");
    assert!(!payload_text.contains("other-model"), "{payload_text}");
    assert!(!payload_text.contains("/model"), "{payload_text}");
    assert!(payload_text.contains("real user question"));
}
