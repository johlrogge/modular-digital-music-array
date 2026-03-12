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

/// Fit a string into exactly `width` chars: pad right with spaces if shorter, truncate with … if longer.
fn fit(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= width {
        format!("{:<width$}", s, width = width)
    } else {
        // truncate to width-1 and add ellipsis
        let truncated: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
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

            // Column widths (chars):
            //   artist:    22
            //   separator: 3  (" — ")
            //   bpm:       8  ("  BBBbpm")
            //   duration:  9  ("  [MM:SS]")
            //   title:     remaining
            const ARTIST_W: usize = 22;
            const SEP_W: usize = 3;
            const BPM_W: usize = 8;
            const DUR_W: usize = 9;
            let title_width = (area.width as usize)
                .saturating_sub(ARTIST_W + SEP_W + BPM_W + DUR_W)
                .max(4);

            let artist_str = fit(artist, ARTIST_W);
            let title_str = fit(title, title_width);
            let bpm_str = track
                .bpm
                .map(|b| format!("  {:>3}bpm", b))
                .unwrap_or_else(|| " ".repeat(BPM_W));
            let dur_str = track
                .duration
                .map(|d| format!("  [{}]", d))
                .unwrap_or_else(|| " ".repeat(DUR_W));

            let spans = vec![
                Span::styled(artist_str, Style::default().fg(artist_color).bg(bg)),
                Span::styled(" — ", Style::default().fg(meta_color).bg(bg)),
                Span::styled(
                    title_str,
                    Style::default()
                        .fg(title_color)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(bpm_str, Style::default().fg(meta_color).bg(bg)),
                Span::styled(dur_str, Style::default().fg(meta_color).bg(bg)),
            ];

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

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_pads_short_string_to_width() {
        let result = fit("hello", 10);
        assert_eq!(result.chars().count(), 10);
        assert_eq!(result, "hello     ");
    }

    #[test]
    fn fit_returns_exact_string_at_width() {
        let result = fit("hello", 5);
        assert_eq!(result.chars().count(), 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn fit_truncates_long_string_with_ellipsis() {
        let result = fit("hello world", 8);
        assert_eq!(result.chars().count(), 8);
        assert!(result.ends_with('…'));
        assert_eq!(result, "hello w…");
    }

    #[test]
    fn fit_width_zero_returns_empty() {
        assert_eq!(fit("hello", 0), "");
    }

    #[test]
    fn fit_width_one_returns_ellipsis_for_long_string() {
        let result = fit("hello", 1);
        assert_eq!(result.chars().count(), 1);
        assert_eq!(result, "…");
    }

    #[test]
    fn fit_handles_multibyte_chars() {
        // "héllo" is 5 chars but more than 5 bytes
        let result = fit("héllo", 5);
        assert_eq!(result.chars().count(), 5);
        assert_eq!(result, "héllo");

        let result = fit("héllo world", 6);
        assert_eq!(result.chars().count(), 6);
        assert!(result.ends_with('…'));
    }
}
