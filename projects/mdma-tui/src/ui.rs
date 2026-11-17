use crate::app::{App, InputMode, PaletteEntry, Side, TABS_PER_SIDE};
use crate::now_playing::PlaybackStatus;
use crate::theme::{
    self, RowState, ACCENT, ACCENT2, BG_SURFACE, BORDER_SUBTLE, MUTED, SELECTION_FG, SUCCESS,
    TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, WARNING,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
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

    // Paint the whole frame first so nothing falls back to the terminal's
    // own default colours.
    theme::paint_background(f, area);

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

    // Terminals without truecolor get the nearest 256-colour palette entry.
    theme::apply_mode(f.buffer_mut(), app.color_mode);
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

            // Paused tracks use explicit muted colours (no terminal-dependent DIM).
            let (artist_fg, sep_fg, title_fg, time_fg) = if is_paused {
                (MUTED, MUTED, TEXT_TERTIARY, MUTED)
            } else {
                (ACCENT2, TEXT_TERTIARY, TEXT_PRIMARY, TEXT_TERTIARY)
            };

            let mut spans = vec![Span::styled(icon, Style::default().fg(icon_color))];
            if !artist.is_empty() {
                spans.push(Span::styled(
                    artist.to_string(),
                    Style::default().fg(artist_fg),
                ));
                spans.push(Span::styled("  —  ", Style::default().fg(sep_fg)));
            }
            spans.push(Span::styled(
                title.to_string(),
                Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(time, Style::default().fg(time_fg)));
            Line::from(spans)
        }
    };

    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(BG_SURFACE)),
        area,
    );
}

/// Render one pane side with an active/inactive border highlight and a tab strip.
fn render_pane_area(f: &mut Frame, app: &App, side: Side, area: Rect) {
    let is_active = app.active_side == side;

    let (tabs, tab_idx, recency) = match side {
        Side::Left => (&app.left_tabs, app.left_tab_idx, &app.left_recency),
        Side::Right => (&app.right_tabs, app.right_tab_idx, &app.right_recency),
    };

    let pane = tabs[tab_idx]
        .as_ref()
        .expect("active tab must be Some")
        .as_ref();

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

    if inner_area.height < 2 {
        // Too small for a tab strip — just render the pane.
        pane.render(f, inner_area);
        return;
    }

    // Split inner_area: 1 row for tab strip + rest for pane content.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner_area);

    let tab_strip_area = chunks[0];
    let pane_area = chunks[1];

    render_tab_strip(
        f,
        app,
        side,
        tabs,
        tab_idx,
        recency,
        is_active,
        tab_strip_area,
    );
    pane.render(f, pane_area);
}

/// Render the tab strip for one side.
///
/// Each slot is labelled with its display key (left: 1-5, right: 6,7,8,9,0).
/// Populated slots show "N:title"; empty slots show "N" in a muted style.
/// Active slot is highlighted. Titles shrink under space pressure in LRU order
/// (least-recently-visited title drops first; active tab's title is last to go).
#[allow(clippy::too_many_arguments)]
fn render_tab_strip(
    f: &mut Frame,
    _app: &App,
    side: Side,
    tabs: &[Option<Box<dyn crate::pane::Pane>>; TABS_PER_SIDE],
    active_idx: usize,
    recency: &[usize],
    _is_active_side: bool,
    area: Rect,
) {
    // Display key for each slot (0-based index → key char displayed to user).
    let display_key = |idx: usize| -> char {
        match side {
            Side::Left => (b'1' + idx as u8) as char, // '1'..'5'
            Side::Right => {
                if idx == 4 {
                    '0'
                } else {
                    (b'6' + idx as u8) as char
                } // '6'..'9','0'
            }
        }
    };

    // Build full label for each slot.
    let labels: Vec<String> = (0..TABS_PER_SIDE)
        .map(|i| {
            let k = display_key(i);
            match &tabs[i] {
                Some(pane) => format!("{}:{}", k, pane.title()),
                None => format!("{}", k),
            }
        })
        .collect();

    // Determine shrink priority: lower number = shrink first.
    // Active tab: highest priority (never shrink). Others: by LRU position (oldest first).
    // Empty slots: lowest priority.
    let priority_of = |idx: usize| -> usize {
        if idx == active_idx {
            return usize::MAX; // never shrink
        }
        if tabs[idx].is_none() {
            return 0; // shrink first
        }
        // Find position in recency vec: position 0 = most recent.
        // Convert: most recent → high priority, least recent → low priority.
        let pos = recency
            .iter()
            .position(|&r| r == idx)
            .unwrap_or(TABS_PER_SIDE);
        // Invert: pos 0 (most recent) → TABS_PER_SIDE priority; pos N-1 → 1 priority.
        TABS_PER_SIDE - pos
    };

    // Available width for the strip (minus separators between tabs).
    // Separators: TABS_PER_SIDE - 1 spaces between cells.
    let available_width = area.width as usize;
    let sep_width = TABS_PER_SIDE - 1; // one space between each pair
    let content_width = available_width.saturating_sub(sep_width);

    // Natural widths (each label as-is).
    let mut widths: Vec<usize> = labels.iter().map(|l| l.chars().count()).collect();
    let total_natural: usize = widths.iter().sum();

    if total_natural > content_width {
        // Need to shrink. Build priority order: sort indices by priority ascending
        // (lowest priority first = shrink first).
        let mut shrink_order: Vec<usize> = (0..TABS_PER_SIDE).collect();
        shrink_order.sort_by_key(|&i| priority_of(i));

        let budget = content_width;
        // First pass: assign minimum widths (just the key char, e.g. "1").
        let min_widths: Vec<usize> = (0..TABS_PER_SIDE)
            .map(|i| {
                let k = display_key(i);
                if tabs[i].is_some() {
                    // minimum: "N:" (2 chars) — key + colon
                    format!("{}:", k).chars().count()
                } else {
                    // minimum: "N" (1 char)
                    format!("{}", k).chars().count()
                }
            })
            .collect();

        // Distribute budget: give each slot at least its minimum.
        let total_min: usize = min_widths.iter().sum();
        if total_min >= budget {
            // Very tight: give everyone just their minimum.
            widths = min_widths;
        } else {
            // Give everyone their minimum first, then distribute remainder to higher-priority slots.
            let mut remaining = budget - total_min;
            widths = min_widths.clone();

            // Distribute from highest priority down (reverse shrink_order).
            for &idx in shrink_order.iter().rev() {
                let natural = labels[idx].chars().count();
                let current = widths[idx];
                let can_add = natural - current;
                let give = can_add.min(remaining);
                widths[idx] += give;
                remaining = remaining.saturating_sub(give);
                if remaining == 0 {
                    break;
                }
            }
        }
    }

    // Build the line spans.
    let mut spans: Vec<Span> = Vec::new();
    for i in 0..TABS_PER_SIDE {
        if i > 0 {
            spans.push(Span::styled(" ", Style::default().fg(TEXT_TERTIARY)));
        }

        let label = &labels[i];
        let w = widths[i];
        let text = truncate_to(label, w);

        let is_active_tab = i == active_idx;
        let style = if is_active_tab {
            Style::default()
                .fg(SELECTION_FG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else if tabs[i].is_some() {
            Style::default().fg(TEXT_SECONDARY)
        } else {
            Style::default().fg(TEXT_TERTIARY)
        };

        spans.push(Span::styled(text, style));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Truncate a string to at most `width` visible characters, appending `…` if truncated.
fn truncate_to(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let len = s.chars().count();
    if len <= width {
        s.to_string()
    } else if width == 1 {
        s.chars().take(1).collect()
    } else {
        let truncated: String = s.chars().take(width - 1).collect();
        format!("{}…", truncated)
    }
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
        InputMode::NameInput => Line::from(vec![
            Span::styled("new playlist: ", Style::default().fg(ACCENT)),
            Span::styled(app.name_input.as_str(), Style::default().fg(TEXT_PRIMARY)),
            Span::styled("_", Style::default().fg(ACCENT)),
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

    // Where the facts come from and how the node is, right-aligned and as wide as
    // it needs; the left side keeps the rest.
    let source = app.source_text().unwrap_or_default();
    let source_width = (source.chars().count() as u16).min(area.width / 2);
    let [left_area, source_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(source_width)])
        .areas(area);

    f.render_widget(
        Paragraph::new(left_line).style(Style::default().bg(BG_SURFACE)),
        left_area,
    );
    f.render_widget(
        Paragraph::new(Span::styled(source, Style::default().fg(TEXT_TERTIARY)))
            .style(Style::default().bg(BG_SURFACE)),
        source_area,
    );
}

/// The bordered block shared by the palette and help overlays.
fn overlay_block<'a>(title: String) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(WARNING))
        .title(Span::styled(
            title,
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        ))
}

/// Most palette matches shown at once.
const MAX_MATCHES: usize = 5;

/// Render the floating command palette overlay.
fn render_palette_overlay(f: &mut Frame, app: &App, area: Rect) {
    let visible_matches = app.palette_matches.len().min(MAX_MATCHES);
    let height = (2 + 1 + visible_matches) as u16;

    let top = area.height.saturating_sub(height + 1);
    let overlay_area = Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: height.min(area.height),
    };

    theme::clear_overlay(f, overlay_area);

    let block = overlay_block(format!(" : {} ", app.palette_query));

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

    render_palette_rows(f, chunks[1], &app.palette_matches, app.palette_cursor);
}

/// Index of the first match shown: the window of `MAX_MATCHES` rows follows
/// the cursor, so the highlighted row is always visible.
fn palette_window_start(cursor: usize) -> usize {
    (cursor + 1).saturating_sub(MAX_MATCHES)
}

fn render_palette_rows(f: &mut Frame, area: Rect, matches: &[PaletteEntry], cursor: usize) {
    if !matches.is_empty() {
        let start = palette_window_start(cursor);
        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .skip(start)
            .take(MAX_MATCHES)
            .map(|(i, entry)| {
                let (label, label_fg, description) = match entry {
                    PaletteEntry::Command(cmd) => {
                        (cmd.name.to_string(), ACCENT2, cmd.description.to_string())
                    }
                    PaletteEntry::OpenPlaylist(name) => (
                        name.as_str().to_string(),
                        SUCCESS,
                        "open playlist".to_string(),
                    ),
                    PaletteEntry::CreatePlaylist(name) => (
                        format!("create: {}", name),
                        ACCENT,
                        "new playlist".to_string(),
                    ),
                    PaletteEntry::History(arg) => (
                        format!("history {}", arg),
                        ACCENT2,
                        "history search".to_string(),
                    ),
                };
                // The cursor row is one solid highlight: text uses the row's own fg.
                let state = RowState::from_flags(i == cursor, false);
                let (label_fg, description_fg) = match state {
                    RowState::Cursor => (SELECTION_FG, SELECTION_FG),
                    _ => (label_fg, TEXT_SECONDARY),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10}", label), Style::default().fg(label_fg)),
                    Span::styled(description, Style::default().fg(description_fg)),
                ]))
                .style(state.style())
            })
            .collect();

        f.render_widget(List::new(items), area);
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
    let dim = |s: &'static str| Span::styled(s, Style::default().fg(MUTED));
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
        Line::from(vec![
            key("  n     "),
            desc("  new playlist (in playlists pane)"),
        ]),
        Line::from(vec![key("  d     "), desc("  remove from queue/playlist")]),
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
        Line::from(vec![key("  b      "), desc(" mark current track liked")]),
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

    theme::clear_overlay(f, overlay_area);

    let block = overlay_block(" Help ".to_string());

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    f.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::COMMANDS;
    use crate::theme::{ColorMode, BACKGROUND};
    use ratatui::{backend::TestBackend, style::Color, Terminal};

    const INDEXED: ColorMode = ColorMode::Indexed256;

    /// Draw the themed background plus the help overlay in the given mode and
    /// return every cell's (fg, bg).
    fn render_help(mode: ColorMode) -> Vec<(Color, Color)> {
        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                theme::paint_background(f, area);
                render_help_overlay(f, area);
                theme::apply_mode(f.buffer_mut(), mode);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|c| (c.fg, c.bg)).collect()
    }

    /// Draw the palette rows with the given cursor and return, for every
    /// highlighted row, its text.
    fn highlighted_palette_rows(cursor: usize) -> Vec<String> {
        let matches: Vec<PaletteEntry> = COMMANDS.iter().map(PaletteEntry::Command).collect();
        let mut terminal = Terminal::new(TestBackend::new(40, MAX_MATCHES as u16)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_palette_rows(f, area, &matches, cursor);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width;
        (0..buffer.area.height)
            .filter(|&y| buffer[(0, y)].bg == theme::CURSOR_BG)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn palette_highlight_stays_visible_when_cursor_passes_the_window() {
        assert!(COMMANDS.len() > 7, "needs more commands than rows");
        for cursor in [0, 4, 5, 6, COMMANDS.len() - 1] {
            let rows = highlighted_palette_rows(cursor);
            assert_eq!(rows.len(), 1, "cursor {cursor}: highlighted rows {rows:?}");
            assert!(
                rows[0].starts_with(COMMANDS[cursor].name),
                "cursor {cursor}: highlighted {:?}",
                rows[0]
            );
        }
    }

    #[test]
    fn truecolor_render_keeps_rgb_and_themes_every_background() {
        let cells = render_help(ColorMode::TrueColor);
        assert!(cells.iter().all(|(_, bg)| *bg == BACKGROUND));
        assert!(cells.iter().any(|(fg, _)| *fg == ACCENT2));
    }

    #[test]
    fn indexed_render_has_no_rgb_and_no_terminal_default_colours() {
        let cells = render_help(INDEXED);
        assert_ne!(cells, render_help(ColorMode::TrueColor));
        let background = theme::adapt(BACKGROUND, INDEXED);
        for (fg, bg) in cells {
            // Converting again changes nothing only if no RGB value is left.
            assert_eq!(theme::adapt(fg, INDEXED), fg);
            assert_eq!(bg, background);
        }
    }
}
