use super::state::{Entry, Screen, State, Status, agent_name, confidence, safe};
use ratatui::{
    Frame,
    crossterm::style::Colored,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, HighlightSpacing, Padding, Paragraph, Row, Table, TableState, Wrap,
    },
};
use std::time::{SystemTime, UNIX_EPOCH};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

fn foreground(color: Color) -> Style {
    // With NO_COLOR, crossterm's color commands become bare SGR resets,
    // clearing bold/dim as well. Avoid requesting those color changes.
    if Colored::ansi_color_disabled_memoized() {
        Style::default()
    } else {
        Style::default().fg(color)
    }
}

fn secondary() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn focus() -> Style {
    // Reset cell colors before inversion so the entire focused row has one
    // treatment. The marker and bold weight also identify focus without color.
    Style::default()
        .fg(Color::Reset)
        .bg(Color::Reset)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        .remove_modifier(Modifier::DIM)
}

fn detail_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::TOP)
        .border_style(foreground(MUTED))
        .title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1))
}

pub fn draw(frame: &mut Frame<'_>, state: &State, history_index: usize, history_len: usize) {
    if frame.area().width < 60 || frame.area().height < 20 {
        frame.render_widget(
            Paragraph::new("roost\n\nResize to at least 60 columns and 20 rows.\nq quit")
                .wrap(Wrap { trim: false }),
            frame.area(),
        );
        return;
    }
    let notice_height = if state.message.is_empty() { 0 } else { 2 };
    let [heading, intro, table, details, notice, keys] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(8 - notice_height),
        Constraint::Length(notice_height),
        Constraint::Length(3),
    ])
    .areas(frame.area().inner(Margin {
        horizontal: 1,
        vertical: 0,
    }));
    draw_heading(frame, state, history_index, history_len, heading);
    let summary = introduction(state);
    let lines: Vec<_> = summary
        .lines()
        .enumerate()
        .map(|(index, text)| {
            let style = if index == 0 {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                secondary()
            };
            Line::styled(text.trim_start(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), intro);
    if state.screen == Screen::Setup {
        draw_setup(frame, state, table, details);
    } else {
        draw_table(frame, state, table);
        draw_details(frame, state.current(), details);
    }
    frame.render_widget(
        Paragraph::new(safe(&state.message))
            .style(foreground(Color::Yellow))
            .wrap(Wrap { trim: false }),
        notice,
    );
    frame.render_widget(Paragraph::new(key_hints(state)), keys);
}

fn draw_heading(frame: &mut Frame<'_>, state: &State, index: usize, count: usize, area: Rect) {
    let subtitle = state.snapshot.as_ref().map_or_else(
        || "Saved terminal sessions".into(),
        |s| {
            format!(
                "Saved {} · {} · snapshot {} of {}",
                age(s.captured_at),
                safe(&s.hostname),
                index + 1,
                count
            )
        },
    );
    let text = vec![
        Line::from(vec![
            Span::styled("roost", foreground(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("  /  {}", screen_title(state.screen)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::styled(subtitle, secondary()),
    ];
    frame.render_widget(Paragraph::new(text), area);
}

fn screen_title(screen: Screen) -> &'static str {
    match screen {
        Screen::Browse => "Saved sessions",
        Screen::Review => "Review restore",
        Screen::Results => "Restore results",
        Screen::Setup => "Automatic capture",
    }
}

fn introduction(state: &State) -> String {
    match state.screen {
        Screen::Browse => {
            let selected = state.selected().len();
            let visible = state.visible().len();
            let filter = if state.query.is_empty() {
                String::new()
            } else {
                format!(" · filter: {}", safe(&state.query))
            };
            format!(
                " {selected} selected · {visible} shown{filter}\n Suggested entries are selected. Space toggles a choice."
            )
        }
        Screen::Review => {
            let uncertain = state
                .batch
                .iter()
                .filter(|&&i| !state.entries[i].suggested())
                .count();
            format!(
                " Open {} new Ghostty tabs?  {uncertain} uncertain selections.\n Includes choices hidden by search.",
                state.batch.len()
            )
        }
        Screen::Results => {
            let submitted = state
                .batch
                .iter()
                .filter(|&&i| state.entries[i].status == Status::Submitted)
                .count();
            format!(
                " {submitted} commands submitted · {} failed\n Each command changes to its saved directory before resuming.",
                state.batch.len() - submitted
            )
        }
        Screen::Setup => {
            if state.setup.as_ref().is_some_and(|s| s.uninstall) {
                " Disable automatic capture?\n Keep binary, snapshots and backups.".into()
            } else {
                " Enable automatic capture?\n Every minute + Claude SessionStart; runs immediately."
                    .into()
            }
        }
    }
}

fn draw_table(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let indices = state.visible();
    if indices.is_empty() {
        let text = if state.snapshot.is_none() {
            "No snapshot to display.\nPress c to capture while agents are open (roost capture).\nUse [ and ] to try another saved snapshot."
        } else {
            "No matching sessions. Press Esc to clear your search."
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(secondary()),
            area,
        );
        return;
    }
    let rows = indices
        .iter()
        .map(|&i| row(&state.entries[i], area.width >= 85));
    let mut widths = vec![
        Constraint::Length(3),
        Constraint::Length(9),
        Constraint::Min(10),
    ];
    let mut headers = vec!["", "Agent", "Conversation"];
    if area.width >= 85 {
        widths.push(Constraint::Percentage(22));
        headers.push("Project");
    }
    widths.push(Constraint::Length(12));
    headers.push("Identity");
    let table = Table::new(rows, widths)
        .column_spacing(1)
        .header(
            Row::new(headers)
                .style(secondary().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(focus());
    let mut selection = selection(state.cursor, area);
    frame.render_stateful_widget(table, area, &mut selection);
}

fn selection(cursor: usize, area: Rect) -> TableState {
    let rows = usize::from(area.height.saturating_sub(2)).max(1);
    TableState::default()
        .with_selected(Some(cursor))
        .with_offset(cursor / rows * rows)
}

fn draw_setup(frame: &mut Frame<'_>, state: &State, area: Rect, details: Rect) {
    let Some(setup) = &state.setup else {
        return;
    };
    let rows = setup.changes.iter().map(|(action, path)| {
        Row::new(vec![
            Cell::from(*action),
            Cell::from(safe(
                &path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy(),
            )),
        ])
    });
    let table = Table::new(rows, [Constraint::Length(18), Constraint::Min(20)])
        .header(
            Row::new(["Change", "File"])
                .style(secondary().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(focus());
    frame.render_stateful_widget(table, area, &mut selection(state.cursor, area));
    if let Some((action, path)) = setup.changes.get(state.cursor) {
        let text = format!(
            "{action}: {}\n\nExisting Claude hooks and other settings are preserved.",
            safe(&path.to_string_lossy())
        );
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(detail_block(" Change details ")),
            details,
        );
    }
}

fn row(entry: &Entry, wide: bool) -> Row<'static> {
    let (label, color) = match &entry.status {
        Status::Ready => (
            confidence(entry.session.process_confidence),
            if entry.suggested() {
                Color::Green
            } else {
                Color::Yellow
            },
        ),
        Status::Unavailable(_) => ("Unavailable", MUTED),
        Status::Submitted => ("Submitted", Color::Green),
        Status::Failed(_) => ("Failed", Color::Red),
    };
    let mut cells = vec![
        Cell::from(if entry.selected { "[x]" } else { "[ ]" }).style(if entry.selected {
            foreground(ACCENT)
        } else {
            secondary()
        }),
        Cell::from(agent_name(entry.session.agent)).style(secondary()),
        Cell::from(safe(entry.title())).style(Style::default().add_modifier(Modifier::BOLD)),
    ];
    if wide {
        let path = entry
            .session
            .cwd
            .file_name()
            .unwrap_or(entry.session.cwd.as_os_str())
            .to_string_lossy();
        cells.push(Cell::from(safe(&path)).style(secondary()));
    }
    cells.push(Cell::from(label).style(foreground(color)));
    Row::new(cells)
}

fn draw_details(frame: &mut Frame<'_>, entry: Option<&Entry>, area: Rect) {
    let Some(entry) = entry else {
        frame.render_widget(detail_block(" Selected session "), area);
        return;
    };
    let tab = entry.session.tab.as_ref().map_or_else(
        || "Tab not identified".into(),
        |tab| format!("Tab: {} ({})", safe(&tab.title), confidence(tab.confidence)),
    );
    let (explanation, explanation_style) = match &entry.status {
        Status::Unavailable(error) => (safe(error), foreground(Color::Yellow)),
        Status::Failed(error) => (
            safe(error),
            foreground(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Status::Submitted => (
            "Submitted to a new tab; check the agent's own prompt.".into(),
            foreground(Color::Green),
        ),
        Status::Ready if !entry.suggested() => (
            "This is a candidate; capture could not establish the selected conversation.".into(),
            foreground(Color::Yellow),
        ),
        Status::Ready => (
            format!(
                "Process identity: {}. {tab}",
                confidence(entry.session.process_confidence)
            ),
            secondary(),
        ),
    };
    let lines = vec![
        Line::from(Span::styled(
            safe(entry.title()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::styled(explanation, explanation_style),
        Line::from(safe(&entry.session.cwd.to_string_lossy())),
        Line::from(Span::styled(
            format!("Session: {}", safe(&entry.session.session_id)),
            secondary(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(detail_block(" Selected session ")),
        area,
    );
}

fn shortcut_line(items: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for &(key, action) in items {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let style = if key == "Enter" {
            foreground(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(key, style));
        spans.push(Span::styled(format!(" {action}"), secondary()));
    }
    Line::from(spans)
}

fn key_hints(state: &State) -> Vec<Line<'static>> {
    if state.searching {
        return vec![
            Line::styled("Type to search", secondary()),
            shortcut_line(&[("Enter", "done"), ("Esc", "clear"), ("Ctrl-C", "quit")]),
        ];
    }
    match state.screen {
        Screen::Browse => vec![
            shortcut_line(&[
                ("Enter", "review"),
                ("Space", "select"),
                ("/", "search"),
                ("a", "suggested"),
                ("n", "none"),
            ]),
            shortcut_line(&[
                ("c", "capture"),
                ("r", "refresh"),
                ("i", "install"),
                ("u", "uninstall"),
            ]),
            shortcut_line(&[
                ("Up/Down", "move"),
                ("[", "older"),
                ("]", "newer"),
                ("Esc", "clear"),
                ("q", "quit"),
            ]),
        ],
        Screen::Review => vec![
            shortcut_line(&[("Enter", "open tabs"), ("Esc", "back")]),
            shortcut_line(&[("Up/Down", "inspect"), ("q", "quit")]),
        ],
        Screen::Results => vec![
            shortcut_line(&[("Enter", "back to sessions"), ("q", "quit")]),
            shortcut_line(&[("Up/Down", "inspect results")]),
        ],
        Screen::Setup => vec![
            shortcut_line(&[("Enter", "confirm setup"), ("Esc", "cancel")]),
            shortcut_line(&[("Up/Down", "inspect paths"), ("q", "quit")]),
        ],
    }
}

fn age(captured: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let Some(seconds) = now.checked_sub(captured) else {
        return "in the future (check clock)".into();
    };
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}
