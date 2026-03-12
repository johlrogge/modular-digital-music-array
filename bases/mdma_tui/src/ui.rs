use crate::app::{App, InputMode, Side};
use crate::now_playing::PlaybackStatus;
use crate::theme::{
    ACCENT, ACCENT2, BG_ELEVATED, BG_SURFACE, BORDER_SUBTLE, SUCCESS, TEXT_PRIMARY, TEXT_SECONDARY,
    TEXT_TERTIARY, WARNING,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

/// Render the full TUI layout.
///
/// ```text
/// │ ▶  Artist — Title                    03:24 / 07:12 │  now-playing bar
/// ┌─ Left Pane ──────────────────┬─ Right Pane ─────────────────┐
/// │                              │                              │
/// └──────────────────────────────┴──────────────────────────────┘
/// │ status / : command                                          │  status bar
/// └──────────────────────────────────────────────────────────────┘
/// ```
pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    // Split vertically: now-playing bar (1) + main content + status bar (1)
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let nowplaying_area = rows[0];
    let main_area = rows[1];
    let status_area = rows[2];

    // Split main area horizontally into two panes
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_area);

    render_now_playing_bar(f, app, nowplaying_area);
    render_pane_area(f, app, Side::Left, cols[0]);
    render_pane_area(f, app, Side::Right, cols[1]);
    render_status_bar(f, app, status_area);

    // Render the command palette overlay on top if active.
    if app.mode == InputMode::Palette {
        render_palette_overlay(f, app, area);
    }

    // Render the help overlay on top if active.
    if app.mode == InputMode::Help {
        render_help_overlay(f, area);
    }
}

fn render_now_playing_bar(f: &mut Frame, app: &App, area: Rect) {
    let np = &app.now_playing;

    let line = match &np.status {
        PlaybackStatus::Stopped => Line::from(vec![Span::styled(
            "  ·  nothing playing",
            Style::default().fg(TEXT_TERTIARY),
        )]),
        PlaybackStatus::Playing {
            position_ms,
            duration_ms,
            ..
        }
        | PlaybackStatus::Paused {
            position_ms,
            duration_ms,
            ..
        } => {
            let is_paused = matches!(&np.status, PlaybackStatus::Paused { .. });
            let (icon, icon_color) = if is_paused {
                ("  ⏸ ", WARNING)
            } else {
                ("  ▶ ", SUCCESS)
            };

            let pos_s = position_ms / 1000;
            let dur_s = duration_ms / 1000;
            let time = format!(
                "  {:02}:{:02} / {:02}:{:02}",
                pos_s / 60,
                pos_s % 60,
                dur_s / 60,
                dur_s % 60,
            );

            let artist = np.artist.as_deref().unwrap_or("");
            let title = np.title.as_deref().unwrap_or("Unknown Title");

            let dim = if is_paused {
                Modifier::DIM
            } else {
                Modifier::empty()
            };

            let mut spans = vec![Span::styled(icon, Style::default().fg(icon_color))];
            if !artist.is_empty() {
                spans.push(Span::styled(
                    artist.to_string(),
                    Style::default().fg(ACCENT2).add_modifier(dim),
                ));
                spans.push(Span::styled(
                    "  —  ",
                    Style::default().fg(TEXT_TERTIARY).add_modifier(dim),
                ));
            }
            spans.push(Span::styled(
                title.to_string(),
                Style::default()
                    .fg(TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD | dim),
            ));
            spans.push(Span::styled(
                time,
                Style::default().fg(TEXT_TERTIARY).add_modifier(dim),
            ));
            Line::from(spans)
        }
    };

    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(BG_SURFACE)),
        area,
    );
}

/// Render one pane side with an active/inactive border highlight.
fn render_pane_area(f: &mut Frame, app: &App, side: Side, area: Rect) {
    let is_active = app.active_side == side;
    let pane = match side {
        Side::Left => app.left_pane.as_ref(),
        Side::Right => app.right_pane.as_ref(),
    };

    let border_style = if is_active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(BORDER_SUBTLE)
    };

    let title_style = if is_active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT_TERTIARY)
    };

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .title(Span::styled(format!(" {} ", pane.title()), title_style));

    let inner_area = outer_block.inner(area);
    f.render_widget(outer_block, area);
    pane.render(f, inner_area);
}

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let left_line = match app.mode {
        InputMode::Palette => Line::from(vec![
            Span::styled(":", Style::default().fg(WARNING)),
            Span::styled(
                app.palette_query.as_str(),
                Style::default().fg(TEXT_PRIMARY),
            ),
        ]),
        InputMode::FilterInput => Line::from(vec![
            Span::styled("filter: ", Style::default().fg(ACCENT)),
            Span::styled(app.filter_input.as_str(), Style::default().fg(TEXT_PRIMARY)),
        ]),
        InputMode::Playback => Line::from(vec![
            Span::styled(
                "[PLAY] ",
                Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "p:play/pause  s:stop  n:next  c:clear  Esc:back",
                Style::default().fg(TEXT_TERTIARY),
            ),
        ]),
        InputMode::Normal | InputMode::Help => {
            if let Some(ref msg) = app.status_message {
                Line::from(Span::styled(msg.as_str(), Style::default().fg(SUCCESS)))
            } else {
                Line::from(Span::styled(
                    "Tab:switch  a:add  q:queue  Q:next  p:playback  s:filter  ?:help  :q:quit",
                    Style::default().fg(TEXT_TERTIARY),
                ))
            }
        }
    };

    f.render_widget(
        Paragraph::new(left_line).style(Style::default().bg(BG_SURFACE)),
        area,
    );
}

/// Render the floating command palette overlay.
fn render_palette_overlay(f: &mut Frame, app: &App, area: Rect) {
    const MAX_MATCHES: usize = 5;

    let visible_matches = app.palette_matches.len().min(MAX_MATCHES);
    let height = (2 + 1 + visible_matches) as u16;

    let top = area.height.saturating_sub(height + 1);
    let overlay_area = Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: height.min(area.height),
    };

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(WARNING))
        .title(Span::styled(
            format!(" : {} ", app.palette_query),
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let input_line = Line::from(vec![
        Span::styled(": ", Style::default().fg(WARNING)),
        Span::styled(
            app.palette_query.as_str(),
            Style::default()
                .fg(TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("_", Style::default().fg(WARNING)),
    ]);
    f.render_widget(Paragraph::new(input_line), chunks[0]);

    if !app.palette_matches.is_empty() {
        let items: Vec<ListItem> = app
            .palette_matches
            .iter()
            .take(MAX_MATCHES)
            .map(|cmd| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10}", cmd.name), Style::default().fg(ACCENT2)),
                    Span::styled(cmd.description, Style::default().fg(TEXT_SECONDARY)),
                ]))
            })
            .collect();

        let list = List::new(items).highlight_style(
            Style::default()
                .bg(BG_ELEVATED)
                .add_modifier(Modifier::REVERSED),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(app.palette_cursor));
        f.render_stateful_widget(list, chunks[1], &mut list_state);
    }
}

fn render_help_overlay(f: &mut Frame, area: Rect) {
    const WIDTH: u16 = 60;

    let key = |s: &'static str| Span::styled(s, Style::default().fg(ACCENT2));
    let desc = |s: &'static str| Span::styled(s, Style::default().fg(TEXT_SECONDARY));
    let header = |s: &'static str| {
        Span::styled(
            s,
            Style::default()
                .fg(TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
    };
    let dim = |s: &'static str| Span::styled(s, Style::default().fg(TEXT_TERTIARY));
    let gap = Line::from("");

    let lines: Vec<Line> = vec![
        Line::from(header("  Navigation                  Selection")),
        Line::from(vec![
            key("  j / ↓ "),
            desc("  cursor down         "),
            key("x"),
            desc("     extend down"),
        ]),
        Line::from(vec![
            key("  k / ↑ "),
            desc("  cursor up           "),
            key("X"),
            desc("     extend up"),
        ]),
        Line::from(vec![
            key("  Tab   "),
            desc("  switch pane         "),
            key("%"),
            desc("     select all"),
        ]),
        Line::from(vec![
            Span::raw("                       "),
            key("Esc"),
            desc("   clear / pop filter"),
        ]),
        gap.clone(),
        Line::from(header("  Actions                     Pane switching")),
        Line::from(vec![
            key("  a     "),
            desc("  add to queue        "),
            key(":search"),
            desc("   search pane"),
        ]),
        Line::from(vec![
            key("  q     "),
            desc("  queue append        "),
            key("Q"),
            desc("     queue next"),
        ]),
        Line::from(vec![
            key("  s     "),
            desc("  filter              "),
            key(":browser"),
            desc("  browser pane"),
        ]),
        Line::from(vec![
            key("  /     "),
            desc("  search              "),
            key(":queue"),
            desc("    queue pane"),
        ]),
        Line::from(vec![
            key("  :     "),
            desc("  command palette     "),
            key(":playlists"),
            desc(" playlist list"),
        ]),
        Line::from(vec![key("  ?     "), desc("  this help")]),
        Line::from(vec![key("  :q    "), desc("  quit")]),
        gap.clone(),
        Line::from(vec![
            Span::raw("  "),
            header("Playback commands: "),
            key(":play"),
            Span::raw("  "),
            key(":pause"),
            Span::raw("  "),
            key(":stop"),
            Span::raw("  "),
            key(":next"),
            Span::raw("  "),
            key(":clear"),
        ]),
        gap.clone(),
        Line::from(header("  In playback mode:")),
        Line::from(vec![
            key("  p / Spc"),
            desc(" play/pause      "),
            key("s"),
            desc("    stop"),
        ]),
        Line::from(vec![
            key("  n      "),
            desc(" next track      "),
            key("c"),
            desc("    clear queue"),
        ]),
        Line::from(vec![key("  Esc    "), desc(" back to normal")]),
        gap.clone(),
        Line::from(vec![
            Span::raw("                    "),
            dim("any key to close"),
        ]),
    ];

    let height = (lines.len() as u16) + 2;

    let x = area.x + area.width.saturating_sub(WIDTH) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let overlay_area = Rect {
        x,
        y,
        width: WIDTH.min(area.width),
        height: height.min(area.height),
    };

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(WARNING))
        .title(Span::styled(
            " Help ",
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}
