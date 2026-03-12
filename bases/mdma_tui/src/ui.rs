use crate::app::{App, InputMode, Side};
use crate::now_playing::PlaybackStatus;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

/// Render the full TUI layout.
///
/// ```text
/// ┌─ Left Pane ──────────────────┬─ Right Pane ─────────────────┐
/// │                              │                              │
/// └──────────────────────────────┴──────────────────────────────┘
/// │ status / : command           │ [Playing] 00:00 / 00:00      │
/// └──────────────────────────────────────────────────────────────┘
/// ```
pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    // Split vertically: main content + one-line status bar
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let main_area = rows[0];
    let status_area = rows[1];

    // Split main area horizontally into two panes
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_area);

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

/// Render one pane side with an active/inactive border highlight.
///
/// The styled outer block provides the border and title; the pane's own
/// render() fills the inner content area. This avoids double-borders.
fn render_pane_area(f: &mut Frame, app: &App, side: Side, area: Rect) {
    let is_active = app.active_side == side;
    let pane = match side {
        Side::Left => app.left_pane.as_ref(),
        Side::Right => app.right_pane.as_ref(),
    };

    let border_style = if is_active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title_style = if is_active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .title(Span::styled(format!(" {} ", pane.title()), title_style));

    // Render the styled block first, then render pane content into its inner area.
    // This way pane.render() never draws its own outer block (it receives inner_area).
    let inner_area = outer_block.inner(area);
    f.render_widget(outer_block, area);

    // Pane renders its content into inner_area. QueuePane draws its own inner
    // block (placeholder / track list) inside this area — that is acceptable
    // for this scaffold; the double-border is cleaned up in Task #3.
    pane.render(f, inner_area);
}

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Left side: hint, status message, or active input prefix
    let left_line = match app.mode {
        InputMode::Palette => Line::from(vec![
            Span::styled(":", Style::default().fg(Color::Yellow)),
            Span::raw(app.palette_query.as_str()),
        ]),
        InputMode::FilterInput => Line::from(vec![
            Span::styled("filter: ", Style::default().fg(Color::Magenta)),
            Span::raw(app.filter_input.as_str()),
        ]),
        InputMode::Playback => Line::from(vec![
            Span::styled(
                "[PLAY] ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "p:play/pause  s:stop  n:next  c:clear  Esc:back",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        InputMode::Normal | InputMode::Help => {
            if let Some(ref msg) = app.status_message {
                Line::from(Span::styled(
                    msg.as_str(),
                    Style::default().fg(Color::Green),
                ))
            } else {
                Line::from(Span::styled(
                    "Tab:switch  a:add  q:queue  Q:next  p:playback  s:filter  ?:help  :q:quit",
                    Style::default().fg(Color::DarkGray),
                ))
            }
        }
    };
    f.render_widget(Paragraph::new(left_line), cols[0]);

    // Right side: now-playing summary
    let np = &app.now_playing;
    let np_text = match &np.status {
        PlaybackStatus::Playing {
            position_ms,
            duration_ms,
            ..
        } => {
            let pos_s = position_ms / 1000;
            let dur_s = duration_ms / 1000;
            format!(
                "[Playing] {:02}:{:02} / {:02}:{:02}",
                pos_s / 60,
                pos_s % 60,
                dur_s / 60,
                dur_s % 60,
            )
        }
        PlaybackStatus::Paused {
            position_ms,
            duration_ms,
            ..
        } => {
            let pos_s = position_ms / 1000;
            let dur_s = duration_ms / 1000;
            format!(
                "[Paused] {:02}:{:02} / {:02}:{:02}",
                pos_s / 60,
                pos_s % 60,
                dur_s / 60,
                dur_s % 60,
            )
        }
        PlaybackStatus::Stopped => "Stopped".to_string(),
    };

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            np_text,
            Style::default().fg(Color::White),
        ))),
        cols[1],
    );
}

/// Render the floating command palette overlay.
///
/// ```text
/// ┌─ : ──────────────────────────────────────────────────────────────────────┐
/// │ > play   Resume or start playback                                         │
/// │   pause  Pause playback                                                   │
/// └───────────────────────────────────────────────────────────────────────────┘
/// ```
///
/// The overlay sits at the bottom of the screen, above the status bar area,
/// and is tall enough to show the input line plus up to 5 matches (capped).
fn render_palette_overlay(f: &mut Frame, app: &App, area: Rect) {
    const MAX_MATCHES: usize = 5;

    let visible_matches = app.palette_matches.len().min(MAX_MATCHES);
    // block border (2) + input line (1) + match rows
    let height = (2 + 1 + visible_matches) as u16;

    // Position the overlay at the bottom of the available area.
    // Clamp so it doesn't exceed the screen.
    let top = area.height.saturating_sub(height + 1); // +1 for status bar
    let overlay_area = Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: height.min(area.height),
    };

    // Clear the region first so the overlay is opaque.
    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            format!(" : {} ", app.palette_query),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // Split inner: first line is the input echo, rest is the match list.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    // Input echo line
    let input_line = Line::from(vec![
        Span::styled(": ", Style::default().fg(Color::Yellow)),
        Span::styled(
            app.palette_query.as_str(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("_", Style::default().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(input_line), chunks[0]);

    // Match list
    if !app.palette_matches.is_empty() {
        let items: Vec<ListItem> = app
            .palette_matches
            .iter()
            .take(MAX_MATCHES)
            .map(|cmd| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<10}", cmd.name),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(cmd.description, Style::default().fg(Color::Gray)),
                ]))
            })
            .collect();

        let list = List::new(items).highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::REVERSED),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(app.palette_cursor));
        f.render_stateful_widget(list, chunks[1], &mut list_state);
    }
}

/// Render a small centred mode picker overlay.
///
/// ```text
/// ┌─ Mode ─────────────────────┐
/// │  p   Playback              │
/// Render a centred floating help overlay listing all key bindings.
fn render_help_overlay(f: &mut Frame, area: Rect) {
    // Fixed overlay dimensions.
    const WIDTH: u16 = 60;

    let key = |s: &'static str| Span::styled(s, Style::default().fg(Color::Cyan));
    let desc = |s: &'static str| Span::styled(s, Style::default().fg(Color::Gray));
    let header = |s: &'static str| {
        Span::styled(
            s,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
    };
    let dim = |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
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

    let height = (lines.len() as u16) + 2; // +2 for block borders

    // Centre the overlay within `area`.
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
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    f.render_widget(Paragraph::new(lines), inner);
}
