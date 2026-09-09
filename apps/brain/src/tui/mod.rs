mod runner;

pub use runner::run_interactive;

use ratatui::{
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

    pub fn set_status_line(&mut self, text: String) {
        self.status_line = text;
    }

    #[must_use]
    pub fn status_line(&self) -> &str {
        &self.status_line
    }
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

fn render_status_line(app: &App, area: Rect, buffer: &mut Buffer) {
    Paragraph::new(app.status_line.as_str()).render(area, buffer);
}
