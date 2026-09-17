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
/// a task write goes through the policy-checked IPC path, the permissions
/// section renders the grants `cortex_daemon_status` reports, and the chat
/// tab degrades explicitly when the fake model endpoint is stopped.
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
    let status = client
        .request(&request("cortex_daemon_status", json!({})))
        .await
        .expect("daemon status through cortexd");

    let mut app = App::new();
    if let cortexd::WireResult::Success { value } = status.result {
        let grants = value["grants"]
            .as_array()
            .expect("grants array")
            .iter()
            .filter_map(|grant| grant.as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(
            grants.contains(&"cortex_task_create".to_owned()),
            "the owner principal must hold the task.create grant"
        );
        app.select_tab(Tab::Settings);
        app.settings_menu_down();
        app.open_settings_section();
        let rows = grants
            .into_iter()
            .map(|name| brain::tui::GrantRow {
                destructive: name.ends_with("_delete"),
                name,
                description: String::new(),
            })
            .collect();
        app.set_grants(rows);
    }
    let screen = render_app(&app);
    assert!(
        screen.contains("cortex_task_create") && screen.contains("[destructive]"),
        "permissions section must render daemon grants, got:\n{screen}"
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

/// The settings editor round-trips a profile edit into `cortexd.toml` and the
/// written file validates against the daemon's own settings loader.
#[tokio::test]
async fn tui_settings_editor_writes_a_valid_config() {
    let harness = Harness::start().await;
    let config_path = harness
        .database_path()
        .parent()
        .expect("parent")
        .join("cortexd.toml");
    let settings = cortexd::LocalSettings::load(&config_path)
        .expect("harness settings parse")
        .expect("harness settings exist");

    let mut app = App::new();
    app.select_tab(Tab::Settings);
    app.set_settings_summary(brain::tui::SettingsSummary {
        config_path: config_path.display().to_string(),
        default_profile: settings.default_profile_id().map(str::to_owned),
        profiles: settings
            .provider_profiles()
            .expect("valid profiles")
            .iter()
            .filter_map(|profile| {
                Some((
                    profile.id().as_str().to_owned(),
                    settings.endpoint_for(profile.id().as_str())?.to_owned(),
                    profile.enabled(),
                ))
            })
            .collect(),
        model_status: "resolved by the daemon at startup".to_owned(),
    });

    app.start_settings_edit();
    app.begin_text(brain::tui::TextPurpose::NewProfileId);
    for character in "secondary".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    app.begin_text(brain::tui::TextPurpose::BaseUrl("secondary".to_owned()));
    for character in "http://127.0.0.1:9000/v1/".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    app.save_settings().expect("valid draft saves");

    let settings = cortexd::LocalSettings::load(&config_path)
        .expect("edited settings parse")
        .expect("edited settings exist");
    let ids: Vec<_> = settings
        .provider_profiles()
        .expect("valid profiles")
        .iter()
        .map(|profile| profile.id().as_str().to_owned())
        .collect();
    assert!(ids.contains(&"secondary".to_owned()));
}
