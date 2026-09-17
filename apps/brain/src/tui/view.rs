use super::{
    App, ChatStatus, GrantRow, SETTINGS_SECTIONS, SettingsEditor, SettingsSection, TAB_LABELS, Tab,
    TextPurpose,
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Widget, Wrap},
};

fn accent() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(title, accent()))
}

fn paragraph(lines: Vec<Line<'_>>, title: &str, area: Rect, buffer: &mut Buffer) {
    Paragraph::new(lines)
        .block(panel(title))
        .wrap(Wrap { trim: false })
        .render(area, buffer);
}

pub fn render(app: &App, area: Rect, buffer: &mut Buffer) {
    if area.is_empty() {
        return;
    }
    if area.width < 48 || area.height < 12 {
        Paragraph::new("Ctrl+C: quit. Enlarge terminal to at least 48 x 12.")
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: true })
            .render(area, buffer);
        return;
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(area);
    render_header(app, header, buffer);
    match app.tab {
        Tab::Chat => render_chat(app, body, buffer),
        Tab::Settings => render_settings(app, body, buffer),
    }
    render_footer(app, footer, buffer);
}

fn render_header(app: &App, area: Rect, buffer: &mut Buffer) {
    let mut spans = if area.width >= 48 {
        vec![Span::styled(" BRAIN  ", accent())]
    } else {
        Vec::new()
    };
    for (label, tab) in TAB_LABELS {
        let style = if tab == app.tab {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    // SCRUM-83: the active model is always visible in the header.
    let model = app.active_model();
    spans.push(Span::styled(
        format!(" · {model}"),
        Style::default().fg(Color::DarkGray),
    ));
    Paragraph::new(Line::from(spans)).render(area, buffer);
}

// Keep the end of a long input visible without splitting UTF-8 or wide cells.
fn input_tail(text: &str, width: u16) -> String {
    let mut start = text.len();
    let mut used = 0;
    for (index, c) in text.char_indices().rev() {
        used += Line::from(c.to_string()).width();
        if used > usize::from(width.saturating_sub(1)) {
            break;
        }
        start = index;
    }
    format!("{}▏", &text[start..])
}

fn input(text: &str, title: &str, area: Rect, buffer: &mut Buffer) {
    Paragraph::new(input_tail(text, area.width.saturating_sub(2)))
        .block(panel(title))
        .render(area, buffer);
}

fn render_chat(app: &App, area: Rect, buffer: &mut Buffer) {
    let [transcript, prompt] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area);
    let mut lines = Vec::new();
    if app.transcript.is_empty() {
        lines.push(Line::styled("Start a conversation", accent()));
        lines.push(Line::from(
            "Type a message below and press Enter. Configure a model in Settings to get started.",
        ));
    }
    for (role, text) in &app.transcript {
        lines.push(Line::styled(
            if role == "user" { "You" } else { "Brain" },
            if role == "user" {
                accent()
            } else {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            },
        ));
        lines.extend(text.lines().map(|line| Line::from(line.to_owned())));
        lines.push(Line::default());
    }
    if app.chat_status == ChatStatus::Waiting {
        if let Some(partial) = app.partial_reply() {
            lines.push(Line::styled(
                "Brain",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.extend(partial.lines().map(|line| Line::from(line.to_owned())));
            lines.push(Line::styled("▌", Style::default().fg(Color::Green)));
        } else {
            lines.push(Line::styled(
                "Thinking... You can browse Settings while waiting.",
                Style::default().fg(Color::Yellow),
            ));
        }
    }
    if let ChatStatus::Degraded(code) = &app.chat_status {
        lines.push(Line::styled(
            format!("DEGRADED: {code}. Open Settings to check your model."),
            Style::default().fg(Color::Yellow),
        ));
    }
    let content = Paragraph::new(lines)
        .block(panel(" Conversation "))
        .wrap(Wrap { trim: false });
    let offset = content
        .line_count(transcript.width.saturating_sub(2))
        .saturating_sub(usize::from(transcript.height));
    content
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0))
        .render(transcript, buffer);
    input(&app.chat_input, " Message · Enter to send ", prompt, buffer);
}

/// The settings tab is a menu (SCRUM-180): Enter opens a section, Esc goes
/// back one level. The models editor keeps its full-screen layout once open.
fn render_settings(app: &App, area: Rect, buffer: &mut Buffer) {
    if let Some(editor) = app.settings_editor() {
        render_editor(editor, area, buffer);
        return;
    }
    match app.settings_menu() {
        Some(SettingsSection::Permissions) => render_permissions(app, area, buffer),
        Some(SettingsSection::Daemon) => render_daemon(app, area, buffer),
        // Models renders through the editor; the bare section is reachable
        // only when no config summary loaded, which the editor needs.
        Some(SettingsSection::Models) | None => render_settings_menu(app, area, buffer),
    }
}

fn render_settings_menu(app: &App, area: Rect, buffer: &mut Buffer) {
    if app.settings_summary().is_none() {
        paragraph(
            vec![Line::styled(
                "Settings unavailable",
                Style::default().fg(Color::Yellow),
            )],
            " Settings ",
            area,
            buffer,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = SETTINGS_SECTIONS
        .iter()
        .map(|(_, title, description)| {
            ListItem::new(vec![
                Line::from(Span::styled(*title, accent())),
                Line::from(Span::styled(
                    *description,
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.settings_cursor()));
    ratatui::widgets::StatefulWidget::render(
        List::new(items)
            .block(panel(" Settings "))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› "),
        area,
        buffer,
        &mut state,
    );
}

fn render_permissions(app: &App, area: Rect, buffer: &mut Buffer) {
    let mut lines = vec![Line::from("Capabilities the daemon granted to this TUI:")];
    if app.grants().is_empty() {
        lines.push(Line::styled(
            "Loading granted capabilities...",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for grant in app.grants() {
        lines.push(render_grant_line(grant));
        lines.push(Line::from(Span::styled(
            format!("    {}", grant.description),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "Read-only: grants change at daemon bootstrap or remote enrollment, not here.",
        Style::default().fg(Color::DarkGray),
    ));
    paragraph(lines, " Settings · Permissions ", area, buffer);
}

fn render_grant_line(grant: &GrantRow) -> Line<'_> {
    if grant.destructive {
        Line::from(vec![
            Span::styled("[destructive] ", Style::default().fg(Color::Red)),
            Span::raw(grant.name.clone()),
        ])
    } else {
        Line::from(Span::raw(grant.name.clone()))
    }
}

fn render_daemon(app: &App, area: Rect, buffer: &mut Buffer) {
    let mut lines = Vec::new();
    if let Some(summary) = app.settings_summary() {
        lines.push(Line::from(format!(
            "model status: {}",
            summary.model_status
        )));
        // The configured vault authority is surfaced so degradation is
        // visible without leaving the TUI.
        lines.push(Line::from(vec![
            Span::styled("vault: ", accent()),
            Span::raw(match app.vault_provider() {
                Some(provider) => format!("{provider} (configured)"),
                None => "not configured".to_owned(),
            }),
        ]));
        lines.push(Line::default());
        lines.push(Line::from(format!("config: {}", summary.config_path)));
        lines.push(Line::from(
            "Tokens are stored in the system keyring. Settings apply when the daemon restarts.",
        ));
    } else {
        lines.push(Line::styled(
            "Settings unavailable",
            Style::default().fg(Color::Yellow),
        ));
    }
    paragraph(lines, " Settings · Daemon ", area, buffer);
}

fn render_editor(editor: &SettingsEditor, area: Rect, buffer: &mut Buffer) {
    let title = if editor.dirty {
        " Settings · Models · unsaved changes "
    } else {
        " Settings · Models "
    };
    let block = panel(title);
    let inner = block.inner(area);
    block.render(area, buffer);
    let [details, rows] = Layout::vertical([
        Constraint::Length(if editor.input.is_some() { 5 } else { 4 }),
        Constraint::Min(0),
    ])
    .areas(inner);
    render_editor_details(editor, details, buffer);
    let mut items = vec![ListItem::new(format!(
        "Default profile: {}",
        editor.default_profile().unwrap_or("(none)")
    ))];
    items.extend(editor.profiles().iter().map(|profile| {
        ListItem::new(vec![
            Line::from(format!(
                "{} [{}]{}",
                profile.id,
                if profile.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                if profile.has_credential || profile.secret_ref.is_some() {
                    " · keyring-ref"
                } else {
                    ""
                }
            )),
            Line::from(profile.base_url.as_str()),
        ])
    }));
    let mut state = ListState::default().with_selected(Some(editor.cursor()));
    ratatui::widgets::StatefulWidget::render(
        List::new(items)
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› "),
        rows,
        buffer,
        &mut state,
    );
}

fn render_provider_picker(selected: usize, area: Rect, buffer: &mut Buffer) {
    let mut lines = vec![Line::styled("Add official provider", accent())];
    for (index, preset) in super::OFFICIAL_PROVIDERS.iter().enumerate() {
        let marker = if index == selected { "› " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{} ", preset.label),
                if index == selected {
                    accent()
                } else {
                    Style::default()
                },
            ),
            Span::raw(preset.base_url),
        ]));
        lines.push(Line::from(format!("    {}", preset.key_hint)));
    }
    lines.push(Line::from(
        "Enter: add · only the API key is needed · Esc: cancel",
    ));
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buffer);
}

fn render_editor_details(editor: &SettingsEditor, area: Rect, buffer: &mut Buffer) {
    if let Some(selected) = editor.provider_picker() {
        render_provider_picker(selected, area, buffer);
        return;
    }
    if let Some(purpose) = editor.pending_purpose() {
        let label = match purpose {
            TextPurpose::DefaultProfile => "Default profile id (empty clears)",
            TextPurpose::NewProfileId => "New profile id · step 1 of 2",
            TextPurpose::BaseUrl(_) => "Endpoint URL",
            TextPurpose::Secret(_) => "API token (hidden · system keyring)",
        };
        let shown = if matches!(purpose, TextPurpose::Secret(_)) {
            "*".repeat(
                editor
                    .pending_text()
                    .unwrap_or("")
                    .chars()
                    .count()
                    .min(usize::from(area.width)),
            )
        } else {
            editor.pending_text().unwrap_or("").to_owned()
        };
        let mut lines = vec![
            Line::styled(label, accent()),
            Line::from(input_tail(&shown, area.width)),
            Line::from("Enter: confirm · Esc: cancel"),
        ];
        if let Some(error) = editor.error() {
            lines.push(Line::styled(
                format!("Error: {error}"),
                Style::default().fg(Color::Red),
            ));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buffer);
        return;
    }
    let mut lines = Vec::new();
    if editor.confirm_discard {
        lines.push(Line::styled(
            "Discard unsaved settings changes?",
            Style::default().fg(Color::Yellow),
        ));
        lines.push(Line::from(
            "Enter: discard · Esc: keep editing. Imported tokens remain in the keyring.",
        ));
    } else if let Some(id) = editor.pending_confirm() {
        lines.push(Line::styled(
            format!("Delete profile `{id}`?"),
            Style::default().fg(Color::Red),
        ));
        lines.push(Line::from("Enter: confirm · Esc: cancel"));
    } else {
        lines.push(Line::from(if editor.cursor() == 0 {
            "Enter: choose default · n: new profile · a: official provider"
        } else {
            "Enter: edit URL · i: import token · t: toggle · d: delete"
        }));
        lines.push(Line::from(
            "Up/Down: select · n: new · a: add NVIDIA NIM · Esc: back",
        ));
        lines.push(Line::from(
            "Confirming an API key or URL saves immediately · w: save other changes",
        ));
        if let Some(error) = editor.error() {
            lines.push(Line::styled(
                format!("Error: {error}"),
                Style::default().fg(Color::Red),
            ));
        } else if let Some(status) = editor.status() {
            lines.push(Line::styled(status, Style::default().fg(Color::Green)));
        } else if editor.dirty {
            lines.push(Line::styled(
                "Unsaved changes · press w to save",
                Style::default().fg(Color::Yellow),
            ));
        }
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buffer);
}

fn render_footer(app: &App, area: Rect, buffer: &mut Buffer) {
    let context = match app.tab {
        Tab::Chat => "Enter: send · Esc: stay in Chat",
        Tab::Settings => {
            if app.settings_editor().is_some() {
                "Enter: edit/confirm · Esc: back to the menu"
            } else if app.settings_menu().is_some() {
                "Esc: back to the menu"
            } else {
                "Enter: open section · e: Models · Esc: Chat"
            }
        }
    };
    let prompt_active = app.tab == Tab::Settings
        && app.settings_editor().is_some_and(|e| {
            e.input.is_some()
                || e.confirm_delete.is_some()
                || e.confirm_discard
                || e.provider_picker().is_some()
        });
    let navigation = if prompt_active {
        "Enter: confirm · Esc: cancel · Ctrl+C: quit"
    } else {
        "Tab/Shift+Tab: switch · Ctrl+C: quit"
    };
    Paragraph::new(vec![
        Line::styled(navigation, accent()),
        Line::from(if app.status_line.is_empty() {
            context.to_owned()
        } else {
            format!("{context} · {}", app.status_line)
        }),
    ])
    .render(area, buffer);
}
