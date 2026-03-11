use crate::selection::SelectionState;
use mdma_client::TrackInfo;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, List, ListItem},
    Frame,
};

/// Format a `TrackInfo` as a single display line.
///
/// Format: `{artist} - {title}  {bpm}bpm  [{duration}]`
pub fn format_track_line(track: &TrackInfo) -> String {
    let artist = track.artist.as_deref().unwrap_or("Unknown Artist");
    let title = track.title.as_deref().unwrap_or("Unknown Title");
    let bpm_part = track.bpm.map(|b| format!("  {}bpm", b)).unwrap_or_default();
    let dur_part = track
        .duration
        .map(|d| format!("  [{}]", d))
        .unwrap_or_default();
    format!("{} - {}{}{}", artist, title, bpm_part, dur_part)
}

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
            let line = format_track_line(track);
            let is_cursor = selection.cursor_position() == Some(vis_idx);
            let is_selected = selection.selected.contains(&vis_idx);

            let style = if is_cursor && is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_cursor {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            ListItem::new(Span::styled(line, style))
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
