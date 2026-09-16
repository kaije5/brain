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

#[test]
fn tiny_and_offset_viewports_do_not_panic() {
    for tab in [Tab::Chat, Tab::Tasks, Tab::Notes, Tab::Settings] {
        let mut app = App::new();
        app.select_tab(tab);
        for width in [0, 1, 12, 40, 80] {
            for height in [0, 1, 2, 3, 6, 24] {
                let area = ratatui::layout::Rect::new(2, 3, width, height);
                let mut buffer = ratatui::buffer::Buffer::empty(area);
                brain::tui::render(&app, area, &mut buffer);
            }
        }
    }
}

#[test]
fn empty_screens_explain_the_next_step_and_quit_shortcut() {
    for (tab, message) in [
        (Tab::Chat, "Start a conversation"),
        (Tab::Tasks, "No tasks"),
        (Tab::Notes, "Type at least 2 characters"),
    ] {
        let mut app = App::new();
        app.select_tab(tab);
        let screen = render_app(&app);
        assert!(screen.contains(message), "{screen}");
        assert!(screen.contains("Ctrl+C"));
    }
}

#[test]
fn long_input_keeps_the_typing_position_visible() {
    let mut app = App::new();
    for c in format!("{}END", "x".repeat(200)).chars() {
        app.push_chat_input(c);
    }
    assert!(render_app(&app).contains("END"));
}

#[test]
fn newest_chat_reply_stays_visible_after_transcript_fills_screen() {
    let mut app = App::new();
    for _ in 0..30 {
        app.push_chat_input('x');
        app.submit_prompt();
        app.receive_agent_reply("An earlier response".to_owned());
    }
    app.receive_agent_reply("The latest reply".to_owned());
    assert!(render_app(&app).contains("The latest reply"));
}

#[test]
fn submitting_while_waiting_preserves_the_next_draft() {
    let mut app = App::new();
    app.push_chat_input('a');
    app.submit_prompt();
    app.push_chat_input('b');
    app.submit_prompt();
    assert_eq!(app.transcript().len(), 1);
    assert_eq!(app.chat_input(), "b");
}

#[test]
fn wrapped_history_keeps_latest_reply_and_failure_visible() {
    let mut app = App::new();
    for _ in 0..20 {
        app.receive_agent_reply("x".repeat(99));
    }
    app.receive_agent_reply("Latest wrapped reply".to_owned());
    assert!(render_app(&app).contains("Latest wrapped reply"));
    app.receive_agent_error("provider_unavailable".to_owned());
    let screen = render_app(&app);
    assert!(screen.contains("DEGRADED"));
    assert!(screen.contains("provider_unavailable"));
}

#[test]
fn loaded_status_does_not_replace_contextual_shortcuts() {
    let mut app = App::new();
    app.select_tab(Tab::Tasks);
    app.set_tasks(vec![("A task".to_owned(), "open".to_owned())]);
    app.set_status_line("1 tasks loaded".to_owned());
    assert!(render_app(&app).contains("r: refresh"));
}

#[test]
fn stale_task_index_renders_the_degraded_banner_above_rows() {
    let mut app = App::new();
    app.tab = Tab::Tasks;
    app.set_task_freshness(Some("stale".to_owned()));
    app.set_tasks(vec![("Write the TUI".to_owned(), "open".to_owned())]);
    let screen = render_app(&app);
    assert!(screen.contains("vault index is stale"));
    assert!(screen.contains("(degraded)"));
    // Rows remain visible below the banner.
    assert!(screen.contains("Write the TUI"));
}

#[test]
fn current_task_index_renders_without_the_degraded_banner() {
    let mut app = App::new();
    app.tab = Tab::Tasks;
    app.set_task_freshness(Some("current".to_owned()));
    app.set_tasks(vec![("Write the TUI".to_owned(), "open".to_owned())]);
    let screen = render_app(&app);
    assert!(!screen.contains("vault index is stale"));
    assert!(screen.contains("Write the TUI"));
}

#[test]
fn empty_tasks_with_a_stale_index_still_explain_the_degraded_state() {
    let mut app = App::new();
    app.tab = Tab::Tasks;
    app.set_task_freshness(Some("stale".to_owned()));
    let screen = render_app(&app);
    assert!(screen.contains("vault index is stale"));
    assert!(screen.contains("No tasks to show"));
}

#[test]
fn conflict_recovery_is_visible_in_the_status_line() {
    let mut app = App::new();
    app.set_status_line("tasks unavailable: conflict (vault changed; refreshing)".to_owned());
    let screen = render_app(&app);
    assert!(screen.contains("conflict"));
    assert!(screen.contains("refreshing"));
    assert!(screen.contains("tasks unavailable"));
}

#[test]
fn settings_tab_shows_the_configured_vault_and_index_state() {
    let mut app = App::new();
    app.tab = Tab::Settings;
    app.set_vault_provider(Some("markdown-vault".to_owned()));
    app.set_task_freshness(Some("stale".to_owned()));
    app.set_settings_summary(brain::tui::SettingsSummary {
        config_path: "C:/data/cortexd.toml".to_owned(),
        default_profile: None,
        profiles: Vec::new(),
        model_status: "ready".to_owned(),
    });
    let screen = render_app(&app);
    assert!(screen.contains("vault: markdown-vault (configured)"));
    assert!(screen.contains("task index: stale (degraded)"));

    let mut app = App::new();
    app.tab = Tab::Settings;
    app.set_settings_summary(brain::tui::SettingsSummary {
        config_path: "C:/data/cortexd.toml".to_owned(),
        default_profile: None,
        profiles: Vec::new(),
        model_status: "ready".to_owned(),
    });
    let screen = render_app(&app);
    assert!(screen.contains("vault: not configured"));
    assert!(screen.contains("task index: current"));
}
