mod runner;

pub use runner::run_interactive;

use crate::local_ops::{LocalOpError, SecretWriter, import_secret, validate_profile_id};
use ratatui::{
    Terminal,
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

/// Top-level TUI tabs. Chat is the default view; everything is a view over
/// the same typed daemon capabilities the one-shot CLI uses — the TUI holds
/// no direct database access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tab {
    Chat,
    Tasks,
    Notes,
    Settings,
}

pub const TAB_LABELS: [(&str, Tab); 4] = [
    ("Chat", Tab::Chat),
    ("Tasks", Tab::Tasks),
    ("Notes", Tab::Notes),
    ("Settings", Tab::Settings),
];

/// Chat readiness. `Degraded` is explicit: when no eligible model is
/// configured or the provider is unavailable, the chat tab says so while the
/// deterministic tabs keep working.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatStatus {
    Ready,
    Waiting,
    Degraded(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsSummary {
    pub config_path: String,
    pub default_profile: Option<String>,
    pub profiles: Vec<(String, String, bool)>,
    pub model_status: String,
}

/// What a pending text edit is for inside the settings editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextPurpose {
    DefaultProfile,
    NewProfileId,
    BaseUrl(String),
    Secret(String),
}

/// One provider profile row in the editor draft. `has_credential` records
/// that a keyring credential exists or was just imported; the raw key itself
/// never enters the draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileDraft {
    pub id: String,
    pub base_url: String,
    pub enabled: bool,
    pub secret_ref: Option<String>,
    pub has_credential: bool,
}

/// Draft state for editing the non-secret settings. Pure state machine: all
/// validation and persistence run through its methods so the flows are fully
/// testable.
#[derive(Debug)]
pub struct SettingsEditor {
    default_profile: Option<String>,
    profiles: Vec<ProfileDraft>,
    cursor: usize,
    input: Option<(TextPurpose, String)>,
    confirm_delete: Option<String>,
    error: Option<String>,
    status: Option<String>,
}

impl SettingsEditor {
    fn from_summary(summary: &SettingsSummary) -> Self {
        Self {
            default_profile: summary.default_profile.clone(),
            profiles: summary
                .profiles
                .iter()
                .map(|(id, base_url, enabled)| ProfileDraft {
                    id: id.clone(),
                    base_url: base_url.clone(),
                    enabled: *enabled,
                    secret_ref: None,
                    has_credential: false,
                })
                .collect(),
            cursor: 0,
            input: None,
            confirm_delete: None,
            error: None,
            status: None,
        }
    }

    #[must_use]
    pub fn default_profile(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    #[must_use]
    pub fn profiles(&self) -> &[ProfileDraft] {
        &self.profiles
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    #[must_use]
    pub fn pending_text(&self) -> Option<&str> {
        self.input.as_ref().map(|(_, buffer)| buffer.as_str())
    }

    #[must_use]
    pub fn pending_confirm(&self) -> Option<&str> {
        self.confirm_delete.as_deref()
    }

    #[must_use]
    pub fn pending_purpose(&self) -> Option<&TextPurpose> {
        self.input.as_ref().map(|(purpose, _)| purpose)
    }

    fn row_count(&self) -> usize {
        self.profiles.len() + 1
    }

    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn down(&mut self) {
        if self.cursor + 1 < self.row_count() {
            self.cursor += 1;
        }
    }

    fn selected_profile(&mut self) -> Option<&mut ProfileDraft> {
        self.profiles.get_mut(self.cursor.saturating_sub(1))
    }

    pub fn toggle_enabled(&mut self) {
        if let Some(profile) = self.selected_profile() {
            profile.enabled = !profile.enabled;
        }
    }

    pub fn begin_delete(&mut self) {
        if self.cursor >= 1
            && let Some(profile) = self.profiles.get(self.cursor - 1)
        {
            self.confirm_delete = Some(profile.id.clone());
        }
    }

    pub fn cancel_pending(&mut self) {
        self.input = None;
        self.confirm_delete = None;
    }

    pub fn begin_text(&mut self, purpose: TextPurpose) {
        self.error = None;
        self.status = None;
        self.input = Some((purpose, String::new()));
    }

    pub fn text_input(&mut self, character: char) {
        if let Some((_, buffer)) = self.input.as_mut() {
            buffer.push(character);
        }
    }

    pub fn text_backspace(&mut self) {
        if let Some((_, buffer)) = self.input.as_mut() {
            buffer.pop();
        }
    }

    /// Confirms the pending non-secret text edit. Secret imports must go
    /// through [`Self::confirm_text_with_store`]; this method refuses them so
    /// no caller can bypass the keyring flow.
    pub fn confirm_text(&mut self) {
        self.confirm_inner(None);
    }

    /// Confirms the pending edit. A `Secret` purpose imports the typed value
    /// into the keyring and stores only the resulting `SecretRef`.
    ///
    /// # Errors
    /// Returns the store failure when the keyring write is rejected.
    pub fn confirm_text_with_store<W: SecretWriter>(
        &mut self,
        store: &W,
    ) -> Result<(), LocalOpError> {
        self.confirm_inner(Some(store));
        if self.error.is_some() {
            Err(LocalOpError::StoreUnavailable)
        } else {
            Ok(())
        }
    }

    fn confirm_inner(&mut self, store: Option<&dyn SecretWriter>) {
        self.error = None;
        if self.input.is_none() {
            if let Some(id) = self.confirm_delete.take()
                && let Some(index) = self.profiles.iter().position(|profile| profile.id == id)
            {
                self.profiles.remove(index);
                if self.default_profile.as_deref() == Some(id.as_str()) {
                    self.default_profile = None;
                }
                self.cursor = self.cursor.min(self.row_count().saturating_sub(1));
            }
            return;
        }
        self.confirm_delete = None;
        let Some((purpose, buffer)) = self.input.take() else {
            return;
        };
        let value = buffer.trim().to_owned();
        match purpose {
            TextPurpose::DefaultProfile => {
                if value.is_empty() {
                    self.default_profile = None;
                } else if self.profiles.iter().any(|profile| profile.id == value) {
                    self.default_profile = Some(value);
                } else {
                    self.error = Some(format!("unknown profile `{value}`"));
                    self.input = Some((TextPurpose::DefaultProfile, value));
                }
            }
            TextPurpose::NewProfileId => {
                if let Err(error) = validate_profile_id(&value) {
                    self.error = Some(error.clone());
                } else if self.profiles.iter().any(|profile| profile.id == value) {
                    self.error = Some(format!("profile `{value}` already exists"));
                } else {
                    self.profiles.push(ProfileDraft {
                        id: value.clone(),
                        base_url: String::new(),
                        enabled: true,
                        secret_ref: None,
                        has_credential: false,
                    });
                    self.cursor = self.row_count() - 1;
                    self.input = Some((TextPurpose::BaseUrl(value), String::new()));
                }
            }
            TextPurpose::BaseUrl(id) => {
                if let Some(profile) = self.profiles.iter_mut().find(|profile| profile.id == id) {
                    if value.is_empty() || value.chars().any(char::is_control) {
                        self.error = Some("base_url must be a non-empty endpoint URL".to_owned());
                    } else {
                        profile.base_url = value;
                    }
                }
            }
            TextPurpose::Secret(id) => {
                let Some(store) = store else {
                    self.error = Some("secret import requires the keyring flow".to_owned());
                    return;
                };
                if value.is_empty() {
                    self.error = Some("empty secret".to_owned());
                    return;
                }
                match import_secret(store, &id, value.as_bytes()) {
                    Ok(secret_ref) => {
                        if let Some(profile) =
                            self.profiles.iter_mut().find(|profile| profile.id == id)
                        {
                            profile.secret_ref = Some(secret_ref);
                            profile.has_credential = true;
                        }
                        self.status = Some(format!("credential imported for `{id}`"));
                    }
                    Err(_) => self.error = Some("platform secret store unavailable".to_owned()),
                }
            }
        }
    }

    /// Serializes the draft and, after validating that the generated file
    /// parses back into routing profiles, atomically replaces `path`.
    ///
    /// # Errors
    /// Returns a message for validation failures; the target file is never
    /// touched unless the serialized draft validates.
    pub fn save(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.error = None;
        if let Some(default_profile) = &self.default_profile
            && !self
                .profiles
                .iter()
                .any(|profile| &profile.id == default_profile)
        {
            let message = format!("default profile `{default_profile}` does not exist");
            self.error = Some(message.clone());
            return Err(message);
        }
        for profile in &self.profiles {
            if let Err(error) = validate_profile_id(&profile.id) {
                let message = format!("profile `{}`: {error}", profile.id);
                self.error = Some(message.clone());
                return Err(message);
            }
            if profile.base_url.is_empty() || profile.base_url.chars().any(char::is_control) {
                let message = format!("profile `{}`: base_url must be set", profile.id);
                self.error = Some(message.clone());
                return Err(message);
            }
        }
        let mut contents = String::from("[models]\n");
        if let Some(default_profile) = &self.default_profile {
            contents.push_str("default_profile = \"");
            contents.push_str(default_profile);
            contents.push_str("\"\n");
        }
        contents.push('\n');
        for profile in &self.profiles {
            contents.push_str("[models.profiles.");
            contents.push_str(&profile.id);
            contents.push_str("]\nbase_url = \"");
            contents.push_str(&profile.base_url);
            contents.push_str("\"\nenabled = ");
            contents.push_str(if profile.enabled { "true" } else { "false" });
            contents.push('\n');
            if let Some(secret_ref) = &profile.secret_ref {
                contents.push_str("secret_ref = \"");
                contents.push_str(secret_ref);
                contents.push_str("\"\n");
            }
            contents.push('\n');
        }
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(std::path::PathBuf::new, std::path::Path::to_path_buf);
        let staging = directory.join(format!(
            ".cortexd.toml.new-{}",
            uuid::Uuid::now_v7().simple()
        ));
        if std::fs::write(&staging, &contents).is_err() {
            return Err("config could not be written".to_owned());
        }
        let valid = cortexd::LocalSettings::load(&staging)
            .ok()
            .flatten()
            .and_then(|settings| settings.provider_profiles().ok())
            .is_some();
        if !valid {
            let _ = std::fs::remove_file(&staging);
            let message = "serialized settings failed validation; file unchanged".to_owned();
            self.error = Some(message.clone());
            return Err(message);
        }
        if std::fs::rename(&staging, path).is_err() {
            let _ = std::fs::remove_file(&staging);
            return Err("config could not be replaced".to_owned());
        }
        self.status = Some("settings saved; the daemon applies them on next start".to_owned());
        Ok(())
    }
}

/// UI state machine. Async daemon calls live in the runner; the state only
/// absorbs their results, which keeps the whole TUI testable without a
/// terminal or a daemon.
#[derive(Debug)]
pub struct App {
    pub tab: Tab,
    should_quit: bool,
    chat_input: String,
    chat_status: ChatStatus,
    transcript: Vec<(String, String)>,
    tasks: Vec<(String, String)>,
    notes_query: String,
    note_results: Vec<String>,
    settings: Option<SettingsSummary>,
    editor: Option<SettingsEditor>,
    status_line: String,
}

impl Default for App {
    fn default() -> Self {
        Self {
            tab: Tab::Chat,
            should_quit: false,
            chat_input: String::new(),
            chat_status: ChatStatus::Ready,
            transcript: Vec::new(),
            tasks: Vec::new(),
            notes_query: String::new(),
            note_results: Vec::new(),
            settings: None,
            editor: None,
            status_line: String::new(),
        }
    }
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Chat => Tab::Tasks,
            Tab::Tasks => Tab::Notes,
            Tab::Notes => Tab::Settings,
            Tab::Settings => Tab::Chat,
        };
    }

    pub fn previous_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Chat => Tab::Settings,
            Tab::Tasks => Tab::Chat,
            Tab::Notes => Tab::Tasks,
            Tab::Settings => Tab::Notes,
        };
    }

    pub fn select_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    pub fn push_chat_input(&mut self, character: char) {
        self.chat_input.push(character);
    }

    pub fn backspace_chat_input(&mut self) {
        self.chat_input.pop();
    }

    #[must_use]
    pub fn chat_input(&self) -> &str {
        &self.chat_input
    }

    /// Stages the current input as a user prompt. The runner performs the
    /// policy-checked `cortex_agent_run` request through the daemon.
    pub fn submit_prompt(&mut self) {
        let prompt = self.chat_input.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        self.chat_input.clear();
        self.transcript.push(("user".to_owned(), prompt));
        self.chat_status = ChatStatus::Waiting;
    }

    #[must_use]
    pub fn pending_prompt(&self) -> Option<String> {
        (self.chat_status == ChatStatus::Waiting && self.transcript.len() % 2 == 1)
            .then(|| self.transcript[self.transcript.len() - 1].1.clone())
    }

    pub fn receive_agent_reply(&mut self, reply: String) {
        self.transcript.push(("assistant".to_owned(), reply));
        self.chat_status = ChatStatus::Ready;
    }

    pub fn receive_agent_error(&mut self, code: String) {
        self.chat_status = ChatStatus::Degraded(code);
        self.transcript
            .push(("assistant".to_owned(), String::new()));
        self.transcript.pop();
    }

    #[must_use]
    pub fn chat_status(&self) -> &ChatStatus {
        &self.chat_status
    }

    #[must_use]
    pub fn transcript(&self) -> &[(String, String)] {
        &self.transcript
    }

    pub fn set_tasks(&mut self, rows: Vec<(String, String)>) {
        self.tasks = rows;
    }

    #[must_use]
    pub fn tasks(&self) -> &[(String, String)] {
        &self.tasks
    }

    pub fn push_note_query(&mut self, character: char) {
        self.notes_query.push(character);
    }

    pub fn backspace_note_query(&mut self) {
        self.notes_query.pop();
    }

    #[must_use]
    pub fn note_query(&self) -> &str {
        &self.notes_query
    }

    /// Consumes the staged note query for the runner's search request.
    pub fn take_note_query(&mut self) -> Option<String> {
        let query = self.notes_query.trim().to_owned();
        (query.len() >= 2).then_some(query)
    }

    pub fn set_note_results(&mut self, results: Vec<String>) {
        self.note_results = results;
    }

    #[must_use]
    pub fn note_results(&self) -> &[String] {
        &self.note_results
    }

    pub fn set_settings_summary(&mut self, summary: SettingsSummary) {
        self.settings = Some(summary);
    }

    #[must_use]
    pub const fn settings_summary(&self) -> Option<&SettingsSummary> {
        self.settings.as_ref()
    }

    pub fn editor_up(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.up();
        }
    }

    pub fn editor_down(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.down();
        }
    }

    pub fn begin_text(&mut self, purpose: TextPurpose) {
        if let Some(editor) = self.editor.as_mut() {
            editor.begin_text(purpose);
        }
    }

    pub fn cancel_pending(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.cancel_pending();
        }
    }

    pub fn begin_delete(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.begin_delete();
        }
    }

    pub fn toggle_enabled(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.toggle_enabled();
        }
    }

    pub fn editor_text_input(&mut self, character: char) {
        if let Some(editor) = self.editor.as_mut() {
            editor.text_input(character);
        }
    }

    pub fn editor_text_backspace(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.text_backspace();
        }
    }

    pub fn confirm_text(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.confirm_text();
        }
    }

    /// # Errors
    /// Propagates the keyring import failure when a secret edit is rejected.
    pub fn confirm_text_with_store<W: SecretWriter>(
        &mut self,
        store: &W,
    ) -> Result<(), LocalOpError> {
        match self.editor.as_mut() {
            Some(editor) => editor.confirm_text_with_store(store),
            None => Ok(()),
        }
    }

    /// Enters settings edit mode, drafting the current summary. Does nothing
    /// when no settings have been loaded.
    pub fn start_settings_edit(&mut self) {
        if let Some(summary) = &self.settings {
            self.editor = Some(SettingsEditor::from_summary(summary));
        }
    }

    pub fn cancel_settings_edit(&mut self) {
        self.editor = None;
    }

    #[must_use]
    pub const fn settings_editor(&self) -> Option<&SettingsEditor> {
        self.editor.as_ref()
    }

    /// Saves the editor draft to the config path from the loaded summary.
    ///
    /// # Errors
    /// Returns the validation failure message when the draft is rejected.
    pub fn save_settings(&mut self) -> Result<(), String> {
        let Some(path) = self
            .settings
            .as_ref()
            .map(|summary| summary.config_path.clone())
        else {
            return Err("no settings loaded".to_owned());
        };
        match self.editor.as_mut() {
            Some(editor) => editor.save(std::path::Path::new(&path)),
            None => Err("not editing".to_owned()),
        }
    }

    pub fn set_status_line(&mut self, text: String) {
        self.status_line = text;
    }

    #[must_use]
    pub fn status_line(&self) -> &str {
        &self.status_line
    }
}

/// Convenience wrapper used by tests: renders one frame into a string.
///
/// # Panics
/// Panics when the test terminal cannot be created or a frame cannot be drawn.
#[must_use]
pub fn render_to_string(app: &App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(app, frame.area(), frame.buffer_mut()))
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

/// Renders one frame. Kept as a free function over `&App` so tests can drive
/// it with a `TestBackend`.
pub fn render(app: &App, area: Rect, buffer: &mut Buffer) {
    let header_height = 3;
    let status_height = 1;
    let body = Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height.saturating_sub(header_height + status_height),
    );
    render_header(app, area, buffer);
    match app.tab {
        Tab::Chat => render_chat(app, body, buffer),
        Tab::Tasks => render_tasks(app, body, buffer),
        Tab::Notes => render_notes(app, body, buffer),
        Tab::Settings => render_settings(app, body, buffer),
    }
    render_status_line(
        app,
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        buffer,
    );
}

fn render_header(app: &App, area: Rect, buffer: &mut Buffer) {
    let spans: Vec<Span> = TAB_LABELS
        .iter()
        .flat_map(|(label, tab)| {
            let style = if *tab == app.tab {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
            };
            vec![Span::styled(format!(" {label} "), style), Span::raw(" ")]
        })
        .collect();
    Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::BOTTOM))
        .render(area, buffer);
}

fn render_chat(app: &App, area: Rect, buffer: &mut Buffer) {
    let input_height = 3;
    let transcript_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(input_height),
    );
    let input_area = Rect::new(
        area.x,
        transcript_area.bottom(),
        area.width,
        area.height.min(input_height),
    );
    let mut lines: Vec<Line> = Vec::new();
    if let ChatStatus::Degraded(code) = &app.chat_status {
        lines.push(Line::from(Span::styled(
            format!("DEGRADED: model inference unavailable ({code}). Tasks, notes and settings keep working."),
            Style::default().fg(ratatui::style::Color::Red),
        )));
    }
    for (role, text) in &app.transcript {
        lines.push(Line::from(Span::styled(
            format!("{role}: {text}"),
            if role == "user" {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )));
    }
    if app.chat_status == ChatStatus::Waiting {
        lines.push(Line::from("agent: …"));
    }
    Paragraph::new(lines).render(transcript_area, buffer);
    Paragraph::new(app.chat_input.as_str())
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title("prompt (Enter to send)"),
        )
        .render(input_area, buffer);
}

fn render_tasks(app: &App, area: Rect, buffer: &mut Buffer) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|(title, status)| ListItem::new(format!("[{status}] {title}")))
        .collect();
    List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Tasks"))
        .render(area, buffer);
}

fn render_notes(app: &App, area: Rect, buffer: &mut Buffer) {
    let input_height = 1;
    let query_area = Rect::new(area.x, area.y, area.width, input_height);
    let results_area = Rect::new(
        area.x,
        area.y + input_height,
        area.width,
        area.height.saturating_sub(input_height),
    );
    Paragraph::new(format!("search: {}", app.notes_query)).render(query_area, buffer);
    let items: Vec<ListItem> = app
        .note_results
        .iter()
        .map(|snippet| ListItem::new(snippet.clone()))
        .collect();
    List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title("Notes and memories"),
        )
        .render(results_area, buffer);
}

fn render_settings(app: &App, area: Rect, buffer: &mut Buffer) {
    if let Some(editor) = app.settings_editor() {
        render_settings_editor(editor, area, buffer);
        return;
    }
    let mut lines: Vec<Line> = Vec::new();
    match &app.settings {
        None => lines.push(Line::from("settings unavailable")),
        Some(summary) => {
            lines.push(Line::from(format!("config: {}", summary.config_path)));
            lines.push(Line::from(format!(
                "default profile: {}",
                summary
                    .default_profile
                    .as_deref()
                    .unwrap_or("(none configured)")
            )));
            lines.push(Line::from(format!(
                "model status: {}",
                summary.model_status
            )));
            lines.push(Line::from("provider profiles:"));
            for (id, base_url, enabled) in &summary.profiles {
                lines.push(Line::from(format!(
                    "  {id} @ {base_url} [{}]",
                    if *enabled { "enabled" } else { "disabled" }
                )));
            }
            lines.push(Line::from(
                "secrets are keyring-backed: import with `brain secret import --profile <id>`.",
            ));
        }
    }
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Settings (cortexd.toml)"),
        )
        .render(area, buffer);
}

fn render_settings_editor(editor: &SettingsEditor, area: Rect, buffer: &mut Buffer) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from("default profile: <none>"));
    for (offset, profile) in std::iter::once(("(default profile)", None)).chain(
        editor
            .profiles()
            .iter()
            .map(|profile| (profile.id.as_str(), Some(profile))),
    ) {
        let _ = offset;
        let _ = profile;
    }
    // Rows: index 0 selects the default profile; indices 1.. select profiles.
    let mut rows: Vec<String> = vec![format!(
        "{} [default profile: {}]",
        if editor.cursor() == 0 { ">" } else { " " },
        editor.default_profile().unwrap_or("(none)")
    )];
    for (index, profile) in editor.profiles().iter().enumerate() {
        let marker = if editor.cursor() == index + 1 {
            ">"
        } else {
            " "
        };
        let credential = if profile.secret_ref.is_some() || profile.has_credential {
            " keyring-ref"
        } else {
            ""
        };
        rows.push(format!(
            "{marker} {} @ {} [{}]{}",
            profile.id,
            profile.base_url,
            if profile.enabled {
                "enabled"
            } else {
                "disabled"
            },
            credential
        ));
    }
    lines.extend(rows.into_iter().map(Line::from));
    if let Some(purpose) = editor.pending_purpose() {
        let prompt = match purpose {
            TextPurpose::DefaultProfile => "default profile id".to_owned(),
            TextPurpose::NewProfileId => "new profile id".to_owned(),
            TextPurpose::BaseUrl(id) => format!("base_url for `{id}`"),
            TextPurpose::Secret(id) => format!("secret for `{id}` (hidden)"),
        };
        let shown = match purpose {
            TextPurpose::Secret(_) => "*".repeat(editor.pending_text().unwrap_or("").len()),
            _ => editor.pending_text().unwrap_or("").to_owned(),
        };
        lines.push(Line::from(Span::styled(
            format!("{prompt}: {shown} (Enter to confirm, Esc to cancel)"),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }
    if editor.pending_purpose().is_none() {
        lines.push(Line::from(
            "Enter: edit · n: new profile · i: import secret · t: enable/disable · d: delete · w: save · Esc: done",
        ));
    }
    if let Some(id) = &editor.confirm_delete {
        lines.push(Line::from(Span::styled(
            format!("delete profile `{id}`? Enter to confirm"),
            Style::default().fg(ratatui::style::Color::Red),
        )));
    }
    if let Some(error) = editor.error() {
        lines.push(Line::from(Span::styled(
            format!("error: {error}"),
            Style::default().fg(ratatui::style::Color::Red),
        )));
    }
    if let Some(status) = editor.status() {
        lines.push(Line::from(status));
    }
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Settings editor (cortexd.toml)"),
        )
        .render(area, buffer);
}

fn render_status_line(app: &App, area: Rect, buffer: &mut Buffer) {
    Paragraph::new(app.status_line.as_str()).render(area, buffer);
}
