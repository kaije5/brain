mod runner;
mod view;

pub use runner::run_interactive;
pub use view::render;

use crate::local_ops::{LocalOpError, SecretWriter, import_secret, validate_profile_id};
use ratatui::Terminal;

/// Top-level TUI tabs. Chat is the default view; everything is a view over
/// the same typed daemon capabilities the one-shot CLI uses — the TUI holds
/// no direct database access. Settings is a menu over user-configurable
/// sections (SCRUM-180); vault task and note overviews live on the CLI and
/// MCP surfaces, not the TUI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tab {
    Chat,
    Settings,
}

pub const TAB_LABELS: [(&str, Tab); 2] = [("Chat", Tab::Chat), ("Settings", Tab::Settings)];

/// One navigable section of the settings tab (SCRUM-180). Enter opens a
/// section from the menu, Esc returns to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsSection {
    /// Provider profiles, the default model and keyring credentials.
    Models,
    /// Capabilities the daemon granted to the local principal (read-only).
    Permissions,
    /// Vault, model status and config location.
    Daemon,
}

/// Menu entries in display order.
pub const SETTINGS_SECTIONS: [(SettingsSection, &str, &str); 3] = [
    (
        SettingsSection::Models,
        "Models",
        "provider profiles, default model and API keys",
    ),
    (
        SettingsSection::Permissions,
        "Permissions",
        "capabilities granted to this TUI",
    ),
    (
        SettingsSection::Daemon,
        "Daemon",
        "vault, model status and config location",
    ),
];

/// One granted capability as shown in the permissions section: the stable
/// mcp name, its documented purpose, and whether it can mutate or destroy
/// vault state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantRow {
    pub name: String,
    pub description: String,
    pub destructive: bool,
}

/// Chat readiness. `Degraded` is explicit: when no eligible model is
/// configured or the provider is unavailable, the chat tab says so while the
/// deterministic tabs keep working.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatStatus {
    Ready,
    Waiting,
    Degraded(String),
}

/// What the runner should do with the staged chat input (SCRUM-83).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputCommand {
    /// A normal agent prompt.
    Agent(String),
    /// `/model`: show the active model and the offered catalog.
    ModelList,
    /// `/model <id>`: switch the agent to another offered model for
    /// subsequent turns.
    ModelSelect(String),
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

/// Official provider presets. The NVIDIA hosted catalog uses the same
/// OpenAI-compatible connector as every other profile: `GET /models` and
/// `POST /chat/completions` are derived from its configured API base.
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
    partial_reply: Option<String>,
    /// The model the agent currently resolves to (SCRUM-83).
    active_model: Option<String>,
    /// Models offered by routing for mid-session selection.
    available_models: Vec<String>,
    /// Provider id of the daemon's configured vault, when present.
    vault_provider: Option<String>,
    /// Capabilities granted to the local principal, from `cortex_daemon_status`.
    grants: Vec<GrantRow>,
    settings: Option<SettingsSummary>,
    /// The settings section currently open; `None` shows the menu.
    settings_menu: Option<SettingsSection>,
    /// Highlighted row of the settings menu.
    settings_cursor: usize,
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
            partial_reply: None,
            active_model: None,
            available_models: Vec::new(),
            vault_provider: None,
            grants: Vec::new(),
            settings: None,
            settings_menu: None,
            settings_cursor: 0,
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
            Tab::Chat => Tab::Settings,
            Tab::Settings => Tab::Chat,
        };
    }

    pub fn previous_tab(&mut self) {
        self.next_tab();
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
        let _ = self.submit_input();
    }

    /// Classifies the staged input and stages it. `/model` interactions are
    /// handled without entering the waiting state: an in-flight generation
    /// cannot be interrupted by a selection attempt (SCRUM-83).
    ///
    /// Returns the command the runner should perform, or `None` when the
    /// input was rejected (in-flight generation) or empty.
    pub fn submit_input(&mut self) -> Option<InputCommand> {
        if self.chat_status == ChatStatus::Waiting {
            return None;
        }
        let input = self.chat_input.trim().to_owned();
        if input.is_empty() {
            return None;
        }
        self.chat_input.clear();
        if input == "/model" {
            self.transcript.push(("user".to_owned(), input));
            return Some(InputCommand::ModelList);
        }
        if let Some(model) = input.strip_prefix("/model ") {
            let model = model.trim().to_owned();
            if model.is_empty() {
                self.transcript
                    .push(("user".to_owned(), "/model".to_owned()));
                return Some(InputCommand::ModelList);
            }
            self.transcript.push(("user".to_owned(), input));
            return Some(InputCommand::ModelSelect(model));
        }
        self.transcript.push(("user".to_owned(), input));
        self.chat_status = ChatStatus::Waiting;
        Some(InputCommand::Agent(
            self.pending_prompt().unwrap_or_default(),
        ))
    }

    /// Records the active model and the models offered by routing.
    pub fn set_active_model(&mut self, active: Option<String>, models: Vec<String>) {
        self.active_model = active;
        self.available_models = models;
    }

    #[must_use]
    pub fn active_model(&self) -> &str {
        self.active_model.as_deref().unwrap_or("none resolved")
    }

    #[must_use]
    pub fn available_models(&self) -> &[String] {
        &self.available_models
    }

    /// Applies a successful mid-session model switch.
    pub fn apply_model_switch(&mut self, model: String) {
        self.active_model = Some(model);
    }

    /// Renders an assistant note into the transcript (model listings,
    /// selection confirmations, and actionable errors).
    pub fn push_assistant_note(&mut self, note: String) {
        self.transcript.push(("assistant".to_owned(), note));
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

    /// Appends one streamed segment to the in-progress assistant reply.
    pub fn receive_agent_partial(&mut self, chunk: &str) {
        self.partial_reply
            .get_or_insert_with(String::new)
            .push_str(chunk);
    }

    /// Finalizes the streamed reply, replacing any accumulated partial text.
    pub fn receive_agent_reply(&mut self, reply: String) {
        self.partial_reply = None;
        self.transcript.push(("assistant".to_owned(), reply));
        self.chat_status = ChatStatus::Ready;
    }

    pub fn receive_agent_error(&mut self, code: String) {
        self.partial_reply = None;
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

    /// The assistant reply currently streaming in, when one is in progress.
    #[must_use]
    pub fn partial_reply(&self) -> Option<&str> {
        self.partial_reply.as_deref()
    }

    /// Records the provider id of the daemon's configured vault, if any.
    pub fn set_vault_provider(&mut self, provider_id: Option<String>) {
        self.vault_provider = provider_id;
    }

    #[must_use]
    pub fn vault_provider(&self) -> Option<&str> {
        self.vault_provider.as_deref()
    }

    /// Replaces the granted-capability rows shown in the permissions section.
    pub fn set_grants(&mut self, grants: Vec<GrantRow>) {
        self.grants = grants;
    }

    #[must_use]
    pub fn grants(&self) -> &[GrantRow] {
        &self.grants
    }

    #[must_use]
    pub const fn settings_menu(&self) -> Option<SettingsSection> {
        self.settings_menu
    }

    #[must_use]
    pub const fn settings_cursor(&self) -> usize {
        self.settings_cursor
    }

    pub fn settings_menu_up(&mut self) {
        self.settings_cursor = self.settings_cursor.saturating_sub(1);
    }

    pub fn settings_menu_down(&mut self) {
        if self.settings_cursor + 1 < SETTINGS_SECTIONS.len() {
            self.settings_cursor += 1;
        }
    }

    /// Opens the highlighted settings section. Models needs the loaded
    /// summary to draft from; without it the section stays closed and the
    /// status line explains why.
    pub fn open_settings_section(&mut self) -> bool {
        let Some((section, _, _)) = SETTINGS_SECTIONS.get(self.settings_cursor) else {
            return false;
        };
        if *section == SettingsSection::Models && self.settings.is_none() {
            "settings unavailable: config could not be loaded".clone_into(&mut self.status_line);
            return false;
        }
        if *section == SettingsSection::Models {
            self.start_settings_edit();
        }
        self.settings_menu = Some(*section);
        true
    }

    /// Returns from a section to the settings menu.
    pub fn close_settings_section(&mut self) {
        self.settings_menu = None;
        if self.settings_cursor >= SETTINGS_SECTIONS.len() {
            self.settings_cursor = SETTINGS_SECTIONS.len().saturating_sub(1);
        }
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
        self.settings_menu = Some(SettingsSection::Models);
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
