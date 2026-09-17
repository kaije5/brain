use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use super::{App, InputCommand, SettingsSummary, Tab};
use crate::DaemonClient;
use cortexd::{LocalSettings, data_directory};

/// Async daemon effects applied back onto the state machine by the loop.
enum Effect {
    Tasks(Result<Vec<(String, String)>, String>),
    /// Active model + catalog snapshot from `cortex_model_list` (SCRUM-83).
    ModelCatalog(Result<serde_json::Value, String>),
    /// Newly active model after `cortex_model_select` (SCRUM-83).
    ModelSelected(Result<String, String>),
    /// Freshness tag of the last vault task list (`current`/`stale`).
    VaultFreshness(Option<String>),
    /// Provider id of the daemon's configured vault, when present.
    VaultProvider(Option<String>),
    Notes(Result<Vec<String>, String>),
    AgentChunk(String),
    Agent(Result<String, String>),
}

/// Full-screen interactive session. All reads and writes go through the
/// authenticated daemon IPC path; the TUI never touches the database directly.
///
/// # Errors
/// Returns a redacted error when the terminal cannot be driven or the daemon
/// is unreachable.
pub async fn run_interactive() -> Result<(), String> {
    let client = crate::DaemonClient::from_environment()
        .map_err(|error| format!("daemon unavailable: {}", error.code()))?;
    let mut terminal = enable_terminal().map_err(|error| error.to_string())?;
    let (sender, receiver) = mpsc::channel::<Effect>();
    let mut app = App::new();
    request_tasks(&client, sender.clone());
    request_vault_status(&client, sender.clone());
    request_settings(&mut app);

    loop {
        if crossterm::event::poll(Duration::from_millis(100)).map_err(|error| error.to_string())?
            && let Event::Key(key) = crossterm::event::read().map_err(|error| error.to_string())?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code, key.modifiers, &client, sender.clone());
        }
        while let Ok(effect) = receiver.try_recv() {
            apply_effect(&mut app, effect, &client);
        }
        terminal
            .draw(|frame| super::render(&app, frame.area(), frame.buffer_mut()))
            .map_err(|error| error.to_string())?;
        if app.should_quit() {
            break;
        }
    }
    disable_terminal(&mut terminal).map_err(|error| error.to_string())?;
    Ok(())
}

fn handle_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    client: &DaemonClient,
    sender: mpsc::Sender<Effect>,
) {
    if code == KeyCode::Char('c') && modifiers == KeyModifiers::CONTROL {
        app.quit();
        return;
    }
    if handle_settings_prompt(app, code, modifiers) {
        return;
    }
    if let KeyCode::Char(c) = code {
        if !text_modifiers(modifiers) {
            return;
        }
        match app.tab {
            Tab::Chat => {
                app.push_chat_input(c);
                return;
            }
            Tab::Notes => {
                app.push_note_query(c);
                return;
            }
            _ => {}
        }
    }
    match code {
        KeyCode::Esc => {
            if app.tab == Tab::Settings && app.settings_editor().is_some() {
                if let Some(editor) = app.editor.as_mut() {
                    if editor.dirty {
                        editor.confirm_discard = true;
                    } else {
                        app.cancel_settings_edit();
                    }
                }
            } else {
                app.select_tab(Tab::Chat);
            }
        }
        KeyCode::BackTab => app.previous_tab(),
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => app.previous_tab(),
        KeyCode::Tab => app.next_tab(),
        KeyCode::Char('1') => app.select_tab(Tab::Chat),
        KeyCode::Char('2') => app.select_tab(Tab::Tasks),
        KeyCode::Char('3') => app.select_tab(Tab::Notes),
        KeyCode::Char('4') => app.select_tab(Tab::Settings),
        KeyCode::Up => app.editor_up(),
        KeyCode::Down => app.editor_down(),
        KeyCode::Char('r') if app.tab == Tab::Tasks => {
            app.set_status_line("Refreshing tasks...".to_owned());
            request_tasks(client, sender);
        }
        KeyCode::Backspace => match app.tab {
            Tab::Chat => app.backspace_chat_input(),
            Tab::Notes => app.backspace_note_query(),
            Tab::Settings => {
                if let Some(editor) = app.settings_editor()
                    && editor.pending_purpose().is_some()
                {
                    app.editor_text_backspace();
                }
            }
            Tab::Tasks => {}
        },
        KeyCode::Enter => match app.tab {
            Tab::Settings => handle_settings_enter(app),
            Tab::Chat => {
                if app.chat_status() != &super::ChatStatus::Waiting
                    && !app.chat_input().trim().is_empty()
                {
                    match app.submit_input() {
                        Some(InputCommand::Agent(_)) => {
                            request_agent_reply(client, sender, app);
                        }
                        Some(InputCommand::ModelList) => {
                            request_model_catalog(client, sender);
                        }
                        Some(InputCommand::ModelSelect(model)) => {
                            request_model_select(client, sender, model);
                        }
                        None => {}
                    }
                }
            }
            Tab::Notes => {
                if let Some(query) = app.take_note_query() {
                    app.set_status_line("Searching notes...".to_owned());
                    request_notes(client, sender, query);
                } else {
                    app.set_status_line("Type at least 2 characters to search.".to_owned());
                }
            }
            Tab::Tasks => {}
        },
        KeyCode::Char(character) => match app.tab {
            Tab::Chat => app.push_chat_input(character),
            Tab::Notes => app.push_note_query(character),
            Tab::Settings => handle_settings_char(app, character),
            Tab::Tasks => {}
        },
        _ => {}
    }
}

fn handle_settings_enter(app: &mut App) {
    let Some(editor) = app.settings_editor() else {
        app.start_settings_edit();
        return;
    };
    if editor.pending_purpose().is_some() || editor.pending_confirm().is_some() {
        // Confirming an API key or endpoint URL completes a usable profile,
        // so the draft persists immediately instead of waiting for `w`.
        let autosave = matches!(
            editor.pending_purpose(),
            Some(super::TextPurpose::Secret(_) | super::TextPurpose::BaseUrl(_))
        );
        let _ = app.confirm_text_with_store(&crate::local_ops::PlatformSecretWriter);
        if autosave
            && app
                .settings_editor()
                .is_some_and(|editor| editor.error().is_none())
            && let Err(message) = app.save_settings()
        {
            app.set_status_line(format!("settings: {message}"));
        }
        return;
    }
    if editor.provider_picker().is_some() {
        app.select_provider();
        return;
    }
    if editor.cursor() == 0 {
        app.begin_text(super::TextPurpose::DefaultProfile);
    } else if let Some(profile) = editor.profiles().get(editor.cursor() - 1) {
        app.begin_text(super::TextPurpose::BaseUrl(profile.id.clone()));
    }
}

fn handle_settings_char(app: &mut App, character: char) {
    let Some(editor) = app.settings_editor() else {
        if character == 'e' {
            app.start_settings_edit();
        }
        return;
    };
    if editor.pending_purpose().is_some() {
        app.editor_text_input(character);
        return;
    }
    match character {
        'n' => app.begin_text(super::TextPurpose::NewProfileId),
        'a' => app.open_provider_picker(),
        'i' => {
            if let Some(index) = editor.cursor().checked_sub(1)
                && let Some(profile) = editor.profiles().get(index)
            {
                app.begin_text(super::TextPurpose::Secret(profile.id.clone()));
            }
        }
        't' => app.toggle_enabled(),
        'd' => app.begin_delete(),
        'w' => match app.save_settings() {
            Ok(()) => {}
            Err(message) => app.set_status_line(format!("settings: {message}")),
        },
        _ => {}
    }
}

fn apply_effect(app: &mut App, effect: Effect, client: &DaemonClient) {
    match effect {
        Effect::Tasks(Ok(rows)) => {
            app.set_status_line(format!("{} tasks loaded", rows.len()));
            app.set_tasks(rows);
        }
        Effect::Tasks(Err(code)) => {
            // Typed provider errors surface with actionable recovery text;
            // the raw code stays machine-parseable in parentheses.
            let recovery = match code.as_str() {
                "conflict" => "vault changed; refreshing",
                "not_found" => "a task vanished; refreshing",
                "permission_denied" => "this principal lacks the task.list grant",
                _ => "check the daemon and vault configuration",
            };
            app.set_status_line(format!("tasks unavailable: {code} ({recovery})"));
            app.set_tasks(Vec::new());
        }
        Effect::VaultFreshness(freshness) => app.set_task_freshness(freshness),
        Effect::VaultProvider(provider) => app.set_vault_provider(provider),
        Effect::Notes(Ok(rows)) => {
            app.set_status_line(format!("{} notes found", rows.len()));
            app.set_note_results(rows);
        }
        Effect::Notes(Err(code)) => app.set_status_line(format!("notes unavailable: {code}")),
        Effect::ModelCatalog(Ok(value)) => {
            // SCRUM-83: surface the active model and the offered catalog as
            // assistant notes plus persistent header state.
            let active = value
                .get("active")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let models = value
                .get("models")
                .and_then(serde_json::Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|model| model.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let note = match &active {
                Some(active) => format!("active model: {active}"),
                None => "no model resolved".to_owned(),
            };
            app.push_assistant_note(note.clone());
            if !models.is_empty() {
                app.push_assistant_note(format!("available models: {}", models.join(", ")));
            }
            app.set_active_model(active, models);
            app.set_status_line(note);
        }
        Effect::ModelCatalog(Err(code)) => {
            app.set_status_line(format!("model catalog unavailable: {code}"));
        }
        Effect::ModelSelected(Ok(model)) => {
            app.apply_model_switch(model.clone());
            app.set_status_line(format!(
                "switched to {model} for subsequent turns (history preserved)"
            ));
        }
        Effect::ModelSelected(Err(code)) => {
            app.set_status_line(format!(
                "model switch rejected: {code} (active model unchanged)"
            ));
        }
        Effect::AgentChunk(chunk) => app.receive_agent_partial(&chunk),
        Effect::Agent(Ok(reply)) => app.receive_agent_reply(reply),
        Effect::Agent(Err(code)) => app.receive_agent_error(code),
    }
    let _ = client;
}

fn request_tasks(client: &DaemonClient, sender: mpsc::Sender<Effect>) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = send_capability(
            &client,
            "cortex_task_list",
            serde_json::json!({ "limit": 50 }),
        )
        .await;
        let mut freshness = None;
        let rows = result.map(|values| {
            // v2 lists are freshness-tagged envelopes of typed task rows; the
            // tag drives the degraded-state banner in the Tasks tab.
            values
                .iter()
                .filter_map(|value| {
                    freshness = value
                        .get("freshness")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    value.get("tasks")?.as_array()
                })
                .flatten()
                .filter_map(|task| {
                    Some((
                        task.get("title")?.as_str()?.to_owned(),
                        task.get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_owned(),
                    ))
                })
                .collect()
        });
        let _ = sender.send(Effect::VaultFreshness(freshness));
        let _ = sender.send(Effect::Tasks(rows));
    });
}

/// Fetches the daemon's configured vault identity for the Settings tab.
fn request_vault_status(client: &DaemonClient, sender: mpsc::Sender<Effect>) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = send_capability(&client, "cortex_daemon_status", serde_json::json!({})).await;
        let provider = result.ok().and_then(|values| {
            values.first().and_then(|status| {
                status["vault"]["configured"]
                    .as_bool()
                    .filter(|configured| *configured)
                    .and_then(|_| status["vault"]["provider_id"].as_str().map(str::to_owned))
            })
        });
        let _ = sender.send(Effect::VaultProvider(provider));
    });
}

fn request_notes(client: &DaemonClient, sender: mpsc::Sender<Effect>, query: String) {
    let client = client.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({"query": query, "limit": 20});
        let result = send_capability(&client, "cortex_knowledge_search", payload.clone()).await;
        let notes = result.map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("snippet").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect()
        });
        let _ = sender.send(Effect::Notes(notes));
    });
}

/// Fetches the active model and the discovered catalog (SCRUM-83).
fn request_model_catalog(client: &DaemonClient, sender: mpsc::Sender<Effect>) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = send_capability(&client, "cortex_model_list", serde_json::json!({})).await;
        let value = result.map(|values| values.first().cloned().unwrap_or(serde_json::Value::Null));
        let _ = sender.send(Effect::ModelCatalog(value));
    });
}

/// Selects a model for subsequent turns. The daemon rejects unknown models
/// without touching the active selection (SCRUM-83).
fn request_model_select(client: &DaemonClient, sender: mpsc::Sender<Effect>, model: String) {
    let client = client.clone();
    tokio::spawn(async move {
        let result = send_capability(
            &client,
            "cortex_model_select",
            serde_json::json!({ "model": model }),
        )
        .await;
        let value = result.map(|values| {
            values
                .first()
                .and_then(|value| value.get("active"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        });
        let _ = sender.send(Effect::ModelSelected(value));
    });
}

fn request_agent_reply(client: &DaemonClient, sender: mpsc::Sender<Effect>, app: &App) {
    let Some(prompt) = app.pending_prompt() else {
        return;
    };
    let client = client.clone();
    tokio::spawn(async move {
        let request = crate::CommandRequest {
            request_id: uuid::Uuid::now_v7(),
            operation_id: uuid::Uuid::now_v7(),
            capability: "cortex_agent_run".to_owned(),
            payload: serde_json::json!({"prompt": prompt}),
        };
        let chunk_sender = sender.clone();
        let on_partial = move |chunk: &str| {
            let _ = chunk_sender.send(Effect::AgentChunk(chunk.to_owned()));
        };
        let result = client
            .request_streaming(request, &on_partial)
            .await
            .map_err(|error| error.code().to_owned())
            .and_then(|response| match response.result {
                cortexd::WireResult::Success { value } => value
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "malformed_agent_reply".to_owned()),
                cortexd::WireResult::Error { code } => Err(code),
            });
        let _ = sender.send(Effect::Agent(result));
    });
}

fn request_settings(app: &mut App) {
    let directory = data_directory();
    let path = directory.join("cortexd.toml");
    let settings = LocalSettings::load(&path).ok().flatten();
    let profiles = settings
        .as_ref()
        .and_then(|settings| settings.provider_profiles().ok())
        .unwrap_or_default();
    let summary = SettingsSummary {
        config_path: path.display().to_string(),
        default_profile: settings
            .as_ref()
            .and_then(LocalSettings::default_profile_id)
            .map(str::to_owned),
        profiles: profiles
            .iter()
            .filter_map(|profile| {
                let base_url = settings.as_ref()?.endpoint_for(profile.id().as_str())?;
                Some((
                    profile.id().as_str().to_owned(),
                    base_url.to_owned(),
                    profile.enabled(),
                ))
            })
            .collect(),
        model_status: "resolved by the daemon at startup (no silent fallback)".to_owned(),
    };
    app.set_settings_summary(summary);
}

async fn send_capability(
    client: &DaemonClient,
    capability: &str,
    payload: serde_json::Value,
) -> Result<Vec<serde_json::Value>, String> {
    let request = crate::CommandRequest {
        request_id: uuid::Uuid::now_v7(),
        operation_id: uuid::Uuid::now_v7(),
        capability: capability.to_owned(),
        payload,
    };
    let response = client
        .request(request)
        .await
        .map_err(|error| error.code().to_owned())?;
    match response.result {
        cortexd::WireResult::Success { value } => match value {
            serde_json::Value::Array(values) => Ok(values),
            value @ serde_json::Value::Object(_) => Ok(vec![value]),
            _ => Ok(Vec::new()),
        },
        cortexd::WireResult::Error { code } => Err(code),
    }
}

fn enable_terminal() -> std::io::Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    use crossterm::ExecutableCommand;
    crossterm::terminal::enable_raw_mode()?;
    std::io::stdout().execute(crossterm::terminal::EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(std::io::stdout()))
}

fn disable_terminal(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> std::io::Result<()> {
    use crossterm::ExecutableCommand;
    crossterm::terminal::disable_raw_mode()?;
    std::io::stdout().execute(crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn text_modifiers(modifiers: KeyModifiers) -> bool {
    let unshifted = modifiers - KeyModifiers::SHIFT;
    // Windows reports AltGr printable characters as Ctrl+Alt.
    unshifted.is_empty() || unshifted == (KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn handle_settings_prompt(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if app.tab == Tab::Settings
        && app
            .settings_editor()
            .is_some_and(|editor| editor.confirm_discard)
    {
        match code {
            KeyCode::Enter => app.cancel_settings_edit(),
            KeyCode::Esc => app.cancel_pending(),
            _ => {}
        }
        return true;
    }
    // A settings prompt owns keyboard focus. Navigation and command letters
    // must not escape the prompt or mutate the profile behind it.
    if app.tab == Tab::Settings
        && let Some(editor) = app.settings_editor()
        && (editor.pending_purpose().is_some()
            || editor.pending_confirm().is_some()
            || editor.provider_picker().is_some())
    {
        match code {
            KeyCode::Esc => app.cancel_pending(),
            KeyCode::Enter => handle_settings_enter(app),
            KeyCode::Up if editor.provider_picker().is_some() => app.editor_up(),
            KeyCode::Down if editor.provider_picker().is_some() => app.editor_down(),
            KeyCode::Backspace => app.editor_text_backspace(),
            KeyCode::Char(c) if text_modifiers(modifiers) => app.editor_text_input(c),
            _ => {}
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::TextPurpose;

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        let (sender, _receiver) = mpsc::channel();
        handle_key(
            app,
            code,
            modifiers,
            &DaemonClient::for_keyboard_tests(),
            sender,
        );
    }

    fn editor() -> App {
        let mut app = App::new();
        app.select_tab(Tab::Settings);
        app.set_settings_summary(SettingsSummary {
            config_path: "unused-keyboard-test-config".to_owned(),
            default_profile: Some("nim".to_owned()),
            profiles: vec![("nim".to_owned(), "https://example.com/v1".to_owned(), true)],
            model_status: "ready".to_owned(),
        });
        app.start_settings_edit();
        app
    }

    #[test]
    fn text_fields_receive_q_digits_and_shifted_characters() {
        for tab in [Tab::Chat, Tab::Notes, Tab::Settings] {
            let mut app = editor();
            app.select_tab(tab);
            if tab == Tab::Settings {
                app.begin_text(TextPurpose::Secret("nim".to_owned()));
            }
            for c in "q1234Q!".chars() {
                press(
                    &mut app,
                    KeyCode::Char(c),
                    if c.is_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                );
            }
            assert!(!app.should_quit(), "text input must never quit");
            assert_eq!(app.tab, tab);
            let text = match tab {
                Tab::Chat => app.chat_input(),
                Tab::Notes => app.note_query(),
                _ => app.settings_editor().unwrap().pending_text().unwrap(),
            };
            assert_eq!(text, "q1234Q!");
            if tab == Tab::Settings {
                assert!(!crate::tui::render_to_string(&app, 100, 30).contains("q1234Q!"));
            }
        }
    }

    #[test]
    fn windows_altgr_characters_reach_text_fields() {
        let mut app = editor();
        app.begin_text(TextPurpose::Secret("nim".to_owned()));
        for c in "@{}\\c".chars() {
            press(
                &mut app,
                KeyCode::Char(c),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            );
        }
        assert!(!app.should_quit());
        assert_eq!(
            app.settings_editor().unwrap().pending_text(),
            Some("@{}\\c")
        );
    }

    #[test]
    fn settings_prompt_keeps_focus_until_cancelled() {
        let mut app = editor();
        app.begin_text(TextPurpose::BaseUrl("nim".to_owned()));
        for code in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Down] {
            press(&mut app, code, KeyModifiers::NONE);
        }
        assert_eq!(app.tab, Tab::Settings);
        assert_eq!(app.settings_editor().unwrap().cursor(), 0);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.settings_editor().unwrap().pending_purpose().is_none());
        assert!(!app.should_quit());
    }

    #[test]
    fn official_provider_picker_flows_straight_to_the_api_key_prompt() {
        let mut app = App::new();
        app.select_tab(Tab::Settings);
        app.set_settings_summary(SettingsSummary {
            config_path: "unused-keyboard-test-config".to_owned(),
            default_profile: None,
            profiles: Vec::new(),
            model_status: "ready".to_owned(),
        });
        app.start_settings_edit();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.settings_editor().unwrap().provider_picker(), Some(0));
        // Command letters must not escape the open picker.
        press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(app.settings_editor().unwrap().provider_picker().is_some());
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let editor = app.settings_editor().unwrap();
        assert!(matches!(
            editor.pending_purpose(),
            Some(TextPurpose::Secret(id)) if id == "nim"
        ));
    }

    #[test]
    fn picker_rejects_a_duplicate_preset_profile() {
        let mut app = editor();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let editor = app.settings_editor().unwrap();
        assert!(editor.provider_picker().is_none());
        assert!(editor.pending_purpose().is_none());
        assert!(editor.error().is_some());
        assert_eq!(editor.profiles().len(), 1);
    }

    #[test]
    fn confirming_a_url_saves_the_draft_without_pressing_w() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("cortexd.toml");
        let mut app = App::new();
        app.select_tab(Tab::Settings);
        app.set_settings_summary(SettingsSummary {
            config_path: path.to_string_lossy().into_owned(),
            default_profile: Some("nim".to_owned()),
            profiles: vec![("nim".to_owned(), "https://old.example/v1".to_owned(), true)],
            model_status: "ready".to_owned(),
        });
        app.start_settings_edit();
        // Cursor row 1: Enter opens the endpoint URL prompt for `nim`.
        app.editor_down();
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        for c in "https://integrate.api.nvidia.com/v1".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let contents = std::fs::read_to_string(&path).expect("draft saved automatically");
        assert!(contents.contains("https://integrate.api.nvidia.com/v1"));
        assert!(
            !app.settings_editor().unwrap().dirty,
            "the autosave clears the dirty flag"
        );
    }

    #[test]
    fn a_rejected_confirm_does_not_autosave() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("cortexd.toml");
        let mut app = App::new();
        app.select_tab(Tab::Settings);
        app.set_settings_summary(SettingsSummary {
            config_path: path.to_string_lossy().into_owned(),
            default_profile: Some("nim".to_owned()),
            profiles: vec![("nim".to_owned(), "https://old.example/v1".to_owned(), true)],
            model_status: "ready".to_owned(),
        });
        app.start_settings_edit();
        app.editor_down();
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        // Confirming an empty endpoint is rejected; nothing may be written.
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!path.exists());
        assert!(app.settings_editor().unwrap().error().is_some());
    }

    #[test]
    fn picker_escape_cancels_without_touching_profiles() {
        let mut app = editor();
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let editor = app.settings_editor().unwrap();
        assert!(editor.provider_picker().is_none());
        assert_eq!(editor.profiles().len(), 1);
        assert!(!app.should_quit());
    }

    #[test]
    fn backtab_and_escape_navigate_without_quitting() {
        let mut app = App::new();
        press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.tab, Tab::Settings);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Chat);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.should_quit());
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit());
    }

    #[test]
    fn delete_confirmation_blocks_unrelated_actions() {
        let mut app = editor();
        app.editor_down();
        app.begin_delete();
        for code in [KeyCode::Char('n'), KeyCode::Char('t'), KeyCode::Tab] {
            press(&mut app, code, KeyModifiers::NONE);
        }
        assert_eq!(app.tab, Tab::Settings);
        let editor = app.settings_editor().unwrap();
        assert!(editor.pending_purpose().is_none());
        assert!(editor.profiles()[0].enabled);
        assert_eq!(editor.pending_confirm(), Some("nim"));
    }

    #[test]
    fn default_row_does_not_toggle_or_import_into_first_profile() {
        let mut app = editor();
        press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        let editor = app.settings_editor().unwrap();
        assert!(editor.profiles()[0].enabled);
        assert!(editor.pending_purpose().is_none());
    }

    #[test]
    fn escape_requires_confirmation_before_discarding_changes() {
        let mut app = editor();
        app.editor_down();
        press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.settings_editor().is_some());
        assert!(crate::tui::render_to_string(&app, 100, 30).contains("Discard unsaved"));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.settings_editor().is_some());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.settings_editor().is_none());
        assert!(!app.should_quit());
    }
}
