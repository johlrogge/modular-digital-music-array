use crate::app::{App, InputMode, Side};
use crate::now_playing::PlaybackStatus;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
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
        InputMode::Command => Line::from(vec![
            Span::styled(":", Style::default().fg(Color::Yellow)),
            Span::raw(app.command_input.as_str()),
        ]),
        InputMode::FilterInput => Line::from(vec![
            Span::styled("filter: ", Style::default().fg(Color::Magenta)),
            Span::raw(app.filter_input.as_str()),
        ]),
        InputMode::Normal => {
            if let Some(ref msg) = app.status_message {
                Line::from(Span::styled(
                    msg.as_str(),
                    Style::default().fg(Color::Green),
                ))
            } else {
                Line::from(Span::styled(
                    "q:quit  Tab:switch  s:filter  ::cmd",
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
