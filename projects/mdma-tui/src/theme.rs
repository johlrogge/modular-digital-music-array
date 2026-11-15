//! The TUI's colour theme.
//!
//! Every colour the TUI draws comes from here, as a semantic role with a
//! 24-bit value. No other module may use named ANSI colours or
//! `Color::Reset`: those come from each terminal's own palette and profile,
//! so the TUI would look different on every terminal.
//!
//! Terminals that do not report truecolor get a 256-colour palette index
//! instead: a hand-picked one for every theme role (see `ROLE_INDEX`), the
//! nearest xterm-256 colour for anything else. See [`ColorMode`] and
//! [`apply_mode`].

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Clear},
    Frame,
};

/// Frame background: painted over the whole frame before anything else.
pub const BACKGROUND: Color = Color::Rgb(22, 22, 34); // #161622
pub const BG_SURFACE: Color = Color::Rgb(28, 28, 42); // #1c1c2a
pub const BG_ELEVATED: Color = Color::Rgb(37, 37, 52); // #252534
pub const TEXT_PRIMARY: Color = Color::Rgb(240, 240, 248); // #f0f0f8
pub const TEXT_SECONDARY: Color = Color::Rgb(184, 184, 208); // #b8b8d0
pub const TEXT_TERTIARY: Color = Color::Rgb(152, 152, 184); // #9898b8
/// Placeholder and hint text ("No playlists", "(no matches)", idle input).
pub const MUTED: Color = Color::Rgb(112, 112, 140); // #70708c
pub const ACCENT: Color = Color::Rgb(236, 136, 244); // #ec88f4 magenta
pub const ACCENT2: Color = Color::Rgb(104, 232, 255); // #68e8ff cyan
pub const SUCCESS: Color = Color::Rgb(96, 232, 184); // #60e8b8 teal
pub const WARNING: Color = Color::Rgb(240, 192, 96); // #f0c060 gold
pub const BORDER_SUBTLE: Color = Color::Rgb(56, 56, 78); // #38384e
/// Text on a highlighted (cursor) row.
pub const SELECTION_FG: Color = Color::Rgb(16, 16, 24); // #101018
/// Background of the cursor row.
pub const CURSOR_BG: Color = ACCENT2;
/// Background of the cursor row when it is also selected.
pub const CURSOR_SELECTED_BG: Color = WARNING;
/// Text of a selected row that is not under the cursor.
pub const SELECTED_FG: Color = WARNING;
/// Text colour of the search input while it is being edited.
pub const INPUT_ACTIVE: Color = WARNING;

/// The 256-colour palette index chosen for every theme role. Chosen by hand
/// so that roles which must contrast stay distinct: the nearest-colour
/// mapping would put the three dark backgrounds on neighbouring greys.
///
/// Every `pub const NAME: Color = Color::Rgb(..)` above needs an entry here;
/// a test enforces it. Roles defined as aliases of another role need none.
macro_rules! role {
    ($name:ident, $index:expr) => {
        (stringify!($name), $name, $index)
    };
}

const ROLE_INDEX: &[(&str, Color, u8)] = &[
    role!(BACKGROUND, 233),
    role!(BG_SURFACE, 235),
    role!(BG_ELEVATED, 237),
    role!(BORDER_SUBTLE, 239),
    role!(TEXT_PRIMARY, 255),
    role!(TEXT_SECONDARY, 146),
    role!(TEXT_TERTIARY, 103),
    role!(MUTED, 60),
    role!(ACCENT, 213),
    role!(ACCENT2, 87),
    role!(SUCCESS, 85),
    role!(WARNING, 221),
    role!(SELECTION_FG, 232),
];

/// How a list row is highlighted. Apply it with `ListItem::style` so the whole
/// row, not just the text, carries the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Normal,
    Selected,
    Cursor,
    CursorSelected,
}

impl RowState {
    pub fn from_flags(is_cursor: bool, is_selected: bool) -> Self {
        match (is_cursor, is_selected) {
            (true, true) => RowState::CursorSelected,
            (true, false) => RowState::Cursor,
            (false, true) => RowState::Selected,
            (false, false) => RowState::Normal,
        }
    }

    pub fn bg(self) -> Color {
        match self {
            RowState::Normal | RowState::Selected => BACKGROUND,
            RowState::Cursor => CURSOR_BG,
            RowState::CursorSelected => CURSOR_SELECTED_BG,
        }
    }

    pub fn style(self) -> Style {
        let base = Style::default().bg(self.bg());
        match self {
            RowState::Normal => base.fg(TEXT_PRIMARY),
            RowState::Selected => base.fg(SELECTED_FG).add_modifier(Modifier::BOLD),
            RowState::Cursor | RowState::CursorSelected => {
                base.fg(SELECTION_FG).add_modifier(Modifier::BOLD)
            }
        }
    }
}

/// Paint `area` with the theme background and default text colour.
pub fn paint_background(f: &mut Frame, area: Rect) {
    f.render_widget(
        Block::default().style(Style::default().fg(TEXT_PRIMARY).bg(BACKGROUND)),
        area,
    );
}

/// Blank an overlay area and repaint it with the theme background
/// (`Clear` alone resets cells to the terminal's default colours).
pub fn clear_overlay(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    paint_background(f, area);
}

/// How theme colours reach the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Indexed256,
}

/// Parse a colour-mode value, as found in `MDMA_TUI_COLORS` or `COLORTERM`.
pub fn parse_mode(value: &str) -> Option<ColorMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "truecolor" | "24bit" => Some(ColorMode::TrueColor),
        "256" => Some(ColorMode::Indexed256),
        _ => None,
    }
}

/// Pure decision: `override_value` is `MDMA_TUI_COLORS`, `colorterm` is
/// `COLORTERM`. A recognised override wins (an unrecognised one is logged and
/// ignored); otherwise truecolor is used only when the terminal says so.
pub fn detect_mode(override_value: Option<&str>, colorterm: Option<&str>) -> ColorMode {
    if let Some(value) = override_value {
        match parse_mode(value) {
            Some(mode) => return mode,
            None => tracing::warn!(
                "ignoring MDMA_TUI_COLORS={value:?}: expected `truecolor`, `24bit` or `256`"
            ),
        }
    }
    match colorterm.and_then(parse_mode) {
        Some(ColorMode::TrueColor) => ColorMode::TrueColor,
        _ => ColorMode::Indexed256,
    }
}

/// The mode for this process, from the environment. Call once at startup.
pub fn mode_from_env() -> ColorMode {
    detect_mode(
        std::env::var("MDMA_TUI_COLORS").ok().as_deref(),
        std::env::var("COLORTERM").ok().as_deref(),
    )
}

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

fn nearest_level(v: u8) -> usize {
    (0..CUBE_LEVELS.len())
        .min_by_key(|&i| (CUBE_LEVELS[i] as i32 - v as i32).abs())
        .unwrap_or(0)
}

/// Nearest xterm-256 palette index (6x6x6 cube or grey ramp) for an RGB value.
pub fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube_rgb = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;

    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let grey_i = ((avg.saturating_sub(8) + 5) / 10).min(23) as u8;
    let grey_v = 8 + 10 * grey_i;
    let grey_rgb = (grey_v, grey_v, grey_v);

    if dist((r, g, b), grey_rgb) < dist((r, g, b), cube_rgb) {
        232 + grey_i
    } else {
        cube_idx as u8
    }
}

/// Convert one colour for the given mode. Only `Rgb` values are touched:
/// theme roles use their table entry, anything else the nearest xterm-256.
pub fn adapt(color: Color, mode: ColorMode) -> Color {
    match (color, mode) {
        (Color::Rgb(r, g, b), ColorMode::Indexed256) => {
            let chosen = ROLE_INDEX.iter().find(|(_, role, _)| *role == color);
            let index = match chosen {
                Some(&(_, _, index)) => index,
                None => rgb_to_xterm256(r, g, b),
            };
            Color::Indexed(index)
        }
        _ => color,
    }
}

/// Convert every cell of a rendered buffer for the given mode.
/// A no-op in truecolor mode.
pub fn apply_mode(buf: &mut Buffer, mode: ColorMode) {
    if mode == ColorMode::TrueColor {
        return;
    }
    for cell in buf.content.iter_mut() {
        cell.fg = adapt(cell.fg, mode);
        cell.bg = adapt(cell.bg, mode);
        cell.underline_color = adapt(cell.underline_color, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(color: Color) -> Color {
        adapt(color, ColorMode::Indexed256)
    }

    fn assert_all_distinct(roles: &[Color]) {
        let indices: Vec<Color> = roles.iter().map(|&c| index_of(c)).collect();
        for (i, a) in indices.iter().enumerate() {
            for (j, b) in indices.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "roles {i} and {j} collide at {a:?}");
            }
        }
    }

    #[test]
    fn colorterm_truecolor_and_24bit_select_truecolor() {
        assert_eq!(detect_mode(None, Some("truecolor")), ColorMode::TrueColor);
        assert_eq!(detect_mode(None, Some("24bit")), ColorMode::TrueColor);
        assert_eq!(detect_mode(None, Some("TrueColor")), ColorMode::TrueColor);
    }

    #[test]
    fn missing_or_other_colorterm_selects_256() {
        assert_eq!(detect_mode(None, None), ColorMode::Indexed256);
        assert_eq!(detect_mode(None, Some("")), ColorMode::Indexed256);
        assert_eq!(detect_mode(None, Some("yes")), ColorMode::Indexed256);
    }

    #[test]
    fn override_beats_colorterm_and_accepts_24bit() {
        assert_eq!(
            detect_mode(Some("256"), Some("truecolor")),
            ColorMode::Indexed256
        );
        assert_eq!(detect_mode(Some("truecolor"), None), ColorMode::TrueColor);
        assert_eq!(detect_mode(Some("24bit"), None), ColorMode::TrueColor);
    }

    #[test]
    fn unknown_override_falls_back_to_colorterm() {
        assert_eq!(
            detect_mode(Some("bogus"), Some("truecolor")),
            ColorMode::TrueColor
        );
        assert_eq!(detect_mode(Some("bogus"), None), ColorMode::Indexed256);
    }

    #[test]
    fn rgb_to_xterm256_known_values() {
        assert_eq!(rgb_to_xterm256(0, 0, 0), 16);
        assert_eq!(rgb_to_xterm256(255, 255, 255), 231);
        assert_eq!(rgb_to_xterm256(255, 0, 0), 196);
        assert_eq!(rgb_to_xterm256(0, 255, 0), 46);
        assert_eq!(rgb_to_xterm256(0, 0, 255), 21);
        // exact grey-ramp value 128 = 8 + 10*12
        assert_eq!(rgb_to_xterm256(128, 128, 128), 244);
        // exact cube colour (95, 135, 175)
        assert_eq!(rgb_to_xterm256(95, 135, 175), 16 + 36 + 12 + 3);
    }

    #[test]
    fn adapt_only_converts_rgb_in_256_mode() {
        assert_eq!(index_of(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(
            adapt(Color::Rgb(255, 0, 0), ColorMode::TrueColor),
            Color::Rgb(255, 0, 0)
        );
        assert_eq!(index_of(Color::Reset), Color::Reset);
    }

    #[test]
    fn theme_roles_use_their_table_entry() {
        assert_eq!(index_of(ACCENT), Color::Indexed(213));
        assert_eq!(index_of(BACKGROUND), Color::Indexed(233));
    }

    #[test]
    fn every_rgb_role_has_a_256_entry() {
        let source = include_str!("theme.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        let mut roles = 0;
        for line in code.lines() {
            let Some(rest) = line.strip_prefix("pub const ") else {
                continue;
            };
            if !rest.contains(": Color = Color::Rgb(") {
                continue;
            }
            let name = rest.split(':').next().unwrap();
            roles += 1;
            assert!(
                ROLE_INDEX.iter().any(|(n, _, _)| *n == name),
                "role {name} has no ROLE_INDEX entry"
            );
        }
        assert!(roles >= ROLE_INDEX.len(), "found only {roles} roles");
    }

    #[test]
    fn contrasting_backgrounds_stay_distinct_in_256_mode() {
        assert_all_distinct(&[BACKGROUND, BG_SURFACE, BG_ELEVATED, BORDER_SUBTLE]);
    }

    #[test]
    fn text_shades_stay_distinct_in_256_mode() {
        assert_all_distinct(&[TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, MUTED]);
    }

    #[test]
    fn row_state_styles_the_whole_row_consistently() {
        assert_eq!(RowState::from_flags(true, true), RowState::CursorSelected);
        assert_eq!(RowState::from_flags(true, false), RowState::Cursor);
        assert_eq!(RowState::from_flags(false, true), RowState::Selected);
        assert_eq!(RowState::from_flags(false, false), RowState::Normal);
        assert_eq!(RowState::Cursor.style().bg, Some(CURSOR_BG));
        assert_eq!(
            RowState::CursorSelected.style().bg,
            Some(CURSOR_SELECTED_BG)
        );
        assert_eq!(RowState::Normal.style().bg, Some(BACKGROUND));
    }

    #[test]
    fn apply_mode_converts_buffer_cells() {
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, Style::default().fg(ACCENT).bg(BACKGROUND));
        apply_mode(&mut buf, ColorMode::Indexed256);
        let cell = &buf[(0, 0)];
        assert_eq!(cell.fg, Color::Indexed(213));
        assert_eq!(cell.bg, Color::Indexed(233));
    }

    fn collect_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                collect_sources(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }

    /// Only this file may name colours. Everything else takes them from the
    /// theme so the look does not depend on the terminal's palette or profile.
    #[test]
    fn no_module_outside_theme_names_a_colour() {
        let mut forbidden: Vec<String> = [
            "Color::",
            "::<Color>",
            "Style::reset",
            ".reset()",
            "::Clear",
            "(Clear)",
            "Clear,",
            ", Clear",
            "{Clear",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let plain = "black red green yellow blue magenta cyan gray grey white dark_gray";
        let light = "light_red light_green light_yellow light_blue light_magenta light_cyan";
        for name in plain.split_whitespace().chain(light.split_whitespace()) {
            forbidden.push(format!(".{name}()"));
            forbidden.push(format!(".on_{name}()"));
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_sources(&src, &mut files);
        assert!(files.len() > 5, "found no sources under {src:?}");

        let mut offenders = Vec::new();
        for file in files {
            if file.file_name().is_some_and(|n| n == "theme.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                for pattern in &forbidden {
                    if line.contains(pattern.as_str()) {
                        offenders.push(format!("{}:{}: {pattern}", file.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "colours must come from theme.rs: {offenders:?}"
        );
    }
}
