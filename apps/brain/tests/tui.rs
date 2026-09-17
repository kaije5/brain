use brain::tui::{App, ChatStatus, GrantRow, SettingsSection, Tab};
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

fn settings_app() -> App {
    let mut app = App::new();
    app.select_tab(Tab::Settings);
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
    app
}

#[test]
fn chat_is_the_default_tab_and_tabs_switch() {
    let app = App::new();
    assert_eq!(app.tab, Tab::Chat);
    let mut app = app;
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
fn settings_menu_lists_navigable_sections() {
    let app = settings_app();
    let screen = render_app(&app);
    assert!(screen.contains("Models"));
    assert!(screen.contains("Permissions"));
    assert!(screen.contains("Daemon"));
}

#[test]
fn models_section_renders_profiles_through_the_editor() {
    let mut app = settings_app();
    app.open_settings_section();
    assert!(app.settings_editor().is_some());
    let screen = render_app(&app);
    assert!(screen.contains("cortexd.toml") || screen.contains("Default profile"));
    assert!(screen.contains("nim"));
}

#[test]
fn permissions_section_lists_grants_and_flags_destructive_ones() {
    let mut app = settings_app();
    app.settings_menu_down();
    app.open_settings_section();
    app.set_grants(vec![
        GrantRow {
            name: "cortex_task_list".to_owned(),
            description: "List tasks".to_owned(),
            destructive: false,
        },
        GrantRow {
            name: "cortex_knowledge_delete".to_owned(),
            description: "Delete knowledge".to_owned(),
            destructive: true,
        },
    ]);
    let screen = render_app(&app);
    assert!(screen.contains("cortex_task_list"));
    assert!(screen.contains("List tasks"));
    assert!(screen.contains("cortex_knowledge_delete"));
    assert!(screen.contains("[destructive]"));
    assert!(screen.contains("Read-only"));
}

#[test]
fn daemon_section_renders_vault_and_config_location() {
    let mut app = settings_app();
    app.set_vault_provider(Some("markdown-vault".to_owned()));
    app.settings_menu_down();
    app.settings_menu_down();
    app.open_settings_section();
    let screen = render_app(&app);
    assert!(screen.contains("vault: markdown-vault (configured)"));
    assert!(screen.contains("C:\\data\\cortexd.toml"));

    let mut app = settings_app();
    app.settings_menu_down();
    app.settings_menu_down();
    app.open_settings_section();
    let screen = render_app(&app);
    assert!(screen.contains("vault: not configured"));
}

#[test]
fn tiny_and_offset_viewports_do_not_panic() {
    for tab in [Tab::Chat, Tab::Settings] {
        for menu in [
            None,
            Some(SettingsSection::Models),
            Some(SettingsSection::Permissions),
            Some(SettingsSection::Daemon),
        ] {
            let mut app = App::new();
            app.select_tab(tab);
            if menu.is_some() {
                app.set_settings_summary(brain::tui::SettingsSummary {
                    config_path: "unused".to_owned(),
                    default_profile: None,
                    profiles: Vec::new(),
                    model_status: "ready".to_owned(),
                });
                app.open_settings_section();
            }
            for width in [0, 1, 12, 40, 80] {
                for height in [0, 1, 2, 3, 6, 24] {
                    let area = ratatui::layout::Rect::new(2, 3, width, height);
                    let mut buffer = ratatui::buffer::Buffer::empty(area);
                    brain::tui::render(&app, area, &mut buffer);
                }
            }
        }
    }
}

#[test]
fn empty_screens_explain_the_next_step_and_quit_shortcut() {
    let mut app = App::new();
    app.select_tab(Tab::Chat);
    let screen = render_app(&app);
    assert!(screen.contains("Start a conversation"), "{screen}");
    assert!(screen.contains("Ctrl+C"));
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
fn settings_without_a_loaded_summary_say_so_instead_of_panicking() {
    let mut app = App::new();
    app.select_tab(Tab::Settings);
    let screen = render_app(&app);
    assert!(screen.contains("Settings unavailable"));
    // Opening a section is refused while nothing loaded.
    assert!(!app.open_settings_section());
    assert_eq!(app.settings_menu(), None);
}
