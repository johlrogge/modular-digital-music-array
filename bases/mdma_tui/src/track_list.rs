use crate::selection::SelectionState;
use crate::theme::{ACCENT2, BG_ELEVATED, TEXT_PRIMARY, TEXT_TERTIARY, WARNING};
use mdma_client::TrackInfo;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem},
    Frame,
};

/// Render a list of tracks respecting the SelectionState's visibility and selection.
///
/// The block title and borders are provided by the caller.
pub fn render_track_list(
    f: &mut Frame,
    area: Rect,
    tracks: &[TrackInfo],
    selection: &SelectionState,
    block: Block,
) {
    let items: Vec<ListItem> = selection
        .visible_to_data
        .iter()
        .enumerate()
        .map(|(vis_idx, &data_idx)| {
            let track = &tracks[data_idx];
            let is_cursor = selection.cursor_position() == Some(vis_idx);
            let is_selected = selection.selected.contains(&vis_idx);

            let artist = track.artist.as_deref().unwrap_or("Unknown Artist");
            let title = track.title.as_deref().unwrap_or("Unknown Title");

            let (artist_color, title_color, meta_color, bg) = if is_cursor && is_selected {
                (Color::Black, Color::Black, Color::Black, WARNING)
            } else if is_cursor {
                (Color::Black, Color::Black, Color::Black, ACCENT2)
            } else if is_selected {
                (WARNING, WARNING, TEXT_TERTIARY, Color::Reset)
            } else {
                (ACCENT2, TEXT_PRIMARY, TEXT_TERTIARY, Color::Reset)
            };

            let mut spans = vec![
                Span::styled(artist.to_string(), Style::default().fg(artist_color).bg(bg)),
                Span::styled("  —  ", Style::default().fg(meta_color).bg(bg)),
                Span::styled(
                    title.to_string(),
                    Style::default()
                        .fg(title_color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ];

            if let Some(bpm) = track.bpm {
                spans.push(Span::styled(
                    format!("  {}bpm", bpm),
                    Style::default().fg(meta_color).bg(bg),
                ));
            }
            if let Some(dur) = track.duration {
                spans.push(Span::styled(
                    format!("  [{}]", dur),
                    Style::default().fg(meta_color).bg(bg),
                ));
            }

            ListItem::new(Line::from(spans)).style(Style::default().bg(if is_cursor {
                BG_ELEVATED
            } else {
                Color::Reset
            }))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default());

    // We pass the list_state by cloning the selected index into a fresh
    // ListState so the List widget scrolls to keep the cursor in view.
    let mut ls = selection.list_state.clone();
    f.render_stateful_widget(list, area, &mut ls);
}
