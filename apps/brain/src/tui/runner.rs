use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::CrosstermBackend};

use super::{App, SettingsSummary, Tab};
use crate::DaemonClient;
use cortexd::{LocalSettings, data_directory};

/// Async daemon effects applied back onto the state machine by the loop.
enum Effect {
    Tasks(Result<Vec<(String, String)>, String>),
    Notes(Result<Vec<String>, String>),
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
    app.set_status_line("Tab: switch · q: quit · Enter: send".to_owned());
    request_tasks(&client, sender.clone());
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
    match code {
        KeyCode::Char('q') if modifiers.is_empty() => app.quit(),
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.quit(),
        KeyCode::Esc => app.quit(),
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => app.previous_tab(),
        KeyCode::Tab => app.next_tab(),
        KeyCode::Char('1') => app.select_tab(Tab::Chat),
        KeyCode::Char('2') => app.select_tab(Tab::Tasks),
        KeyCode::Char('3') => app.select_tab(Tab::Notes),
        KeyCode::Char('4') => app.select_tab(Tab::Settings),
        KeyCode::Backspace => match app.tab {
            Tab::Chat => app.backspace_chat_input(),
            Tab::Notes => app.backspace_note_query(),
            _ => {}
        },
        KeyCode::Enter => match app.tab {
            Tab::Chat => {
                app.submit_prompt();
                request_agent_reply(client, sender, app);
            }
            Tab::Notes => {
                if let Some(query) = app.take_note_query() {
                    request_notes(client, sender, query);
                }
            }
            _ => {}
        },
        KeyCode::Char(character) => match app.tab {
            Tab::Chat => app.push_chat_input(character),
            Tab::Notes => app.push_note_query(character),
            _ => {}
        },
        _ => {}
    }
}

fn apply_effect(app: &mut App, effect: Effect, client: &DaemonClient) {
    match effect {
        Effect::Tasks(Ok(rows)) => app.set_tasks(rows),
        Effect::Tasks(Err(code)) => app.set_status_line(format!("tasks unavailable: {code}")),
        Effect::Notes(Ok(rows)) => app.set_note_results(rows),
        Effect::Notes(Err(code)) => app.set_status_line(format!("notes unavailable: {code}")),
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
        let rows = result.map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    Some((
                        value.get("title")?.as_str()?.to_owned(),
                        value
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_owned(),
                    ))
                })
                .collect()
        });
        let _ = sender.send(Effect::Tasks(rows));
    });
}

fn request_notes(client: &DaemonClient, sender: mpsc::Sender<Effect>, query: String) {
    let client = client.clone();
    tokio::spawn(async move {
        let payload = serde_json::json!({"query": query, "limit": 20});
        let result = send_capability(&client, "cortex_note_search", payload.clone()).await;
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

fn request_agent_reply(client: &DaemonClient, sender: mpsc::Sender<Effect>, app: &App) {
    let Some(prompt) = app.pending_prompt() else {
        return;
    };
    let client = client.clone();
    tokio::spawn(async move {
        let result = send_capability(
            &client,
            "cortex_agent_run",
            serde_json::json!({"prompt": prompt}),
        )
        .await;
        let reply = result.and_then(|mut values| {
            values
                .pop()
                .ok_or_else(|| "malformed_agent_reply".to_owned())
                .and_then(|value| {
                    value
                        .get("reply")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| "malformed_agent_reply".to_owned())
                })
        });
        let _ = sender.send(Effect::Agent(reply));
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
