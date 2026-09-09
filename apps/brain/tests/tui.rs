use brain::tui::{App, ChatStatus, Tab};
use ratatui::{Terminal, backend::TestBackend};

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

#[test]
fn chat_is_the_default_tab_and_tabs_switch() {
    let app = App::new();
    assert_eq!(app.tab, Tab::Chat);
    let mut app = app;
    app.next_tab();
    assert_eq!(app.tab, Tab::Tasks);
    app.next_tab();
    assert_eq!(app.tab, Tab::Notes);
    app.next_tab();
    assert_eq!(app.tab, Tab::Settings);
    app.next_tab();
    assert_eq!(app.tab, Tab::Chat);
    app.previous_tab();
    assert_eq!(app.tab, Tab::Settings);
}

#[test]
fn typed_chat_input_is_staged_before_sending() {
    let mut app = App::new();
    for character in "hello agent".chars() {
        app.push_chat_input(character);
    }
    assert_eq!(app.chat_input(), "hello agent");
    app.backspace_chat_input();
    assert_eq!(app.chat_input(), "hello agen");
}

#[test]
fn sending_a_prompt_appends_it_to_the_transcript_and_sets_waiting() {
    let mut app = App::new();
    for character in "hi".chars() {
        app.push_chat_input(character);
    }
    app.submit_prompt();
    assert_eq!(app.chat_input(), "");
    assert_eq!(app.transcript().len(), 1);
    assert_eq!(app.transcript()[0].0, "user");
    assert_eq!(app.chat_status(), &ChatStatus::Waiting);
}

#[test]
fn agent_response_is_appended_and_chat_returns_to_ready() {
    let mut app = App::new();
    for character in "hi".chars() {
        app.push_chat_input(character);
    }
    app.submit_prompt();
    app.receive_agent_reply("All done.".to_owned());
    assert_eq!(app.transcript().len(), 2);
    assert_eq!(app.transcript()[1].0, "assistant");
    assert_eq!(app.transcript()[1].1, "All done.");
    assert_eq!(app.chat_status(), &ChatStatus::Ready);
}

#[test]
fn unavailable_model_shows_an_explicit_degraded_state() {
    let mut app = App::new();
    app.submit_prompt();
    app.receive_agent_error("unavailable".to_owned());
    assert!(matches!(app.chat_status(), ChatStatus::Degraded(_)));
    let screen = render_app(&app);
    assert!(
        screen.to_lowercase().contains("degraded"),
        "chat tab must surface the degraded state explicitly"
    );
}

#[test]
fn tasks_tab_renders_fetched_rows_without_db_access() {
    let mut app = App::new();
    app.tab = Tab::Tasks;
    app.set_tasks(vec![
        ("Write the TUI".to_owned(), "open".to_owned()),
        ("Ship SCRUM-67".to_owned(), "open".to_owned()),
    ]);
    let screen = render_app(&app);
    assert!(screen.contains("Write the TUI"));
    assert!(screen.contains("Ship SCRUM-67"));
}

#[test]
fn notes_tab_renders_search_results() {
    let mut app = App::new();
    app.tab = Tab::Notes;
    app.set_note_results(vec!["Cortex keeps canonical state".to_owned()]);
    let screen = render_app(&app);
    assert!(screen.contains("Cortex keeps canonical state"));
}

#[test]
fn settings_tab_renders_non_secret_config_and_profile() {
    let mut app = App::new();
    app.tab = Tab::Settings;
    app.set_settings_summary(brain::tui::SettingsSummary {
        config_path: "C:\\data\\cortexd.toml".to_owned(),
        default_profile: Some("nim".to_owned()),
        profiles: vec![
            ("nim".to_owned(), "https://nim.example/v1/".to_owned(), true),
            (
                "local".to_owned(),
                "http://127.0.0.1:8000/v1/".to_owned(),
                false,
            ),
        ],
        model_status: "degraded: provider_unavailable".to_owned(),
    });
    let screen = render_app(&app);
    assert!(screen.contains("cortexd.toml"));
    assert!(screen.contains("nim"));
    assert!(screen.contains("degraded"));
}
