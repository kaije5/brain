mod runner;
mod view;

pub use runner::run_interactive;
pub use view::render;

use crate::local_ops::{LocalOpError, SecretWriter, import_secret, validate_profile_id};
use ratatui::Terminal;

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

/// A built-in provider adapter the settings editor offers as a preset: the
/// endpoint is fixed and documented, so the only user input is the API key.
#[derive(Clone, Copy, Debug)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub key_hint: &'static str,
}

/// Official provider presets. NVIDIA NIM's hosted catalog is `OpenAI`
/// compatible (`GET /v1/models`, `POST /v1/chat/completions`) at
/// `integrate.api.nvidia.com/v1`, authenticating with a `nvapi-...` bearer
/// key issued by build.nvidia.com — the contract the `NimDiscovery` adapter
/// already implements.
pub const OFFICIAL_PROVIDERS: [ProviderPreset; 1] = [ProviderPreset {
    id: "nim",
    label: "NVIDIA NIM",
    base_url: "https://integrate.api.nvidia.com/v1",
    key_hint: "API key (nvapi-...) from build.nvidia.com",
}];

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
    provider_picker: Option<usize>,
    error: Option<String>,
    status: Option<String>,
    dirty: bool,
    confirm_discard: bool,
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
            provider_picker: None,
            error: None,
            status: None,
            dirty: false,
            confirm_discard: false,
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

    /// Index of the highlighted official provider, when the picker is open.
    #[must_use]
    pub const fn provider_picker(&self) -> Option<usize> {
        self.provider_picker
    }

    fn row_count(&self) -> usize {
        self.profiles.len() + 1
    }

    pub fn up(&mut self) {
        if let Some(index) = self.provider_picker.as_mut() {
            *index = (*index).saturating_sub(1).min(OFFICIAL_PROVIDERS.len() - 1);
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    pub fn down(&mut self) {
        if let Some(index) = self.provider_picker.as_mut() {
            if *index + 1 < OFFICIAL_PROVIDERS.len() {
                *index += 1;
            }
        } else if self.cursor + 1 < self.row_count() {
            self.cursor += 1;
        }
    }

    fn selected_profile(&mut self) -> Option<&mut ProfileDraft> {
        self.profiles.get_mut(self.cursor.checked_sub(1)?)
    }

    pub fn toggle_enabled(&mut self) {
        if let Some(profile) = self.selected_profile() {
            profile.enabled = !profile.enabled;
            self.dirty = true;
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
        self.provider_picker = None;
        self.confirm_discard = false;
    }

    /// Opens the official provider picker. Selecting a preset creates the
    /// profile with its documented endpoint and jumps straight to the API
    /// key prompt, so the key is the only required input.
    pub fn open_provider_picker(&mut self) {
        if self.provider_picker.is_none() {
            self.error = None;
            self.status = None;
            self.provider_picker = Some(0);
        }
    }

    /// Confirms the picker: adds the highlighted preset as a new profile,
    /// makes it the default when no default exists, and starts the keyring
    /// import prompt for its API key.
    pub fn select_provider(&mut self) {
        let Some(index) = self.provider_picker.take() else {
            return;
        };
        let Some(preset) = OFFICIAL_PROVIDERS.get(index) else {
            return;
        };
        self.error = None;
        self.status = None;
        if self.profiles.iter().any(|profile| profile.id == preset.id) {
            self.error = Some(format!("profile `{}` already exists", preset.id));
            return;
        }
        self.profiles.push(ProfileDraft {
            id: preset.id.to_owned(),
            base_url: preset.base_url.to_owned(),
            enabled: true,
            secret_ref: None,
            has_credential: false,
        });
        if self.default_profile.is_none() {
            self.default_profile = Some(preset.id.to_owned());
        }
        self.dirty = true;
        self.cursor = self.row_count() - 1;
        self.input = Some((TextPurpose::Secret(preset.id.to_owned()), String::new()));
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
                self.dirty = true;
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
                    self.dirty = true;
                } else if self.profiles.iter().any(|profile| profile.id == value) {
                    self.default_profile = Some(value);
                    self.dirty = true;
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
                    self.dirty = true;
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
                        self.dirty = true;
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
                            self.dirty = true;
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
        self.dirty = false;
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
        if self.chat_status == ChatStatus::Waiting {
            return;
        }
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
        (self.chat_status == ChatStatus::Waiting
            && self
                .transcript
                .last()
                .is_some_and(|(role, _)| role == "user"))
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

    pub fn open_provider_picker(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.open_provider_picker();
        }
    }

    pub fn select_provider(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.select_provider();
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
        let editor = self
            .editor
            .as_mut()
            .ok_or_else(|| "not editing".to_owned())?;
        editor.save(std::path::Path::new(&path))?;
        if let Some(summary) = self.settings.as_mut() {
            summary.default_profile.clone_from(&editor.default_profile);
            summary.profiles = editor
                .profiles
                .iter()
                .map(|p| (p.id.clone(), p.base_url.clone(), p.enabled))
                .collect();
        }
        "Settings saved; restart the daemon to apply.".clone_into(&mut self.status_line);
        Ok(())
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
