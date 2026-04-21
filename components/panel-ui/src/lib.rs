use core::fmt::Write as _;
use heapless::String;
use panel_protocol::{Direction, Edge, InputEvent, RenderCommand};

/// The active screen in the UI hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    NowPlaying,
    MainMenu,
    Queue,
    Library,
}

/// Top-level UI state machine.
///
/// Call [`UiState::handle`] with each incoming [`InputEvent`];
/// it returns a list of [`RenderCommand`]s describing the new screen state.
pub struct UiState {
    pub screen: Screen,
    pub selection: usize,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: Screen::NowPlaying,
            selection: 0,
        }
    }
}

impl UiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one input event and return the render commands for the updated screen.
    pub fn handle(&mut self, ev: InputEvent) -> heapless::Vec<RenderCommand, 16> {
        match ev {
            // Tilt ↑ → go to main menu
            InputEvent::EncoderTilt {
                dir: Direction::Up,
                edge: Edge::Press,
            } => {
                self.screen = Screen::MainMenu;
                self.selection = 0;
                self.render()
            }

            // Tilt ↓ → placeholder PLAY/PAUSE (emits a Text command for now)
            InputEvent::EncoderTilt {
                dir: Direction::Down,
                edge: Edge::Press,
            } => {
                let mut cmds: heapless::Vec<RenderCommand, 16> = heapless::Vec::new();
                let mut s: String<64> = String::new();
                let _ = s.push_str("[PLAY/PAUSE]");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 0,
                    s,
                });
                cmds
            }

            // Tilt ← → placeholder PREV
            InputEvent::EncoderTilt {
                dir: Direction::Left,
                edge: Edge::Press,
            } => {
                let mut cmds: heapless::Vec<RenderCommand, 16> = heapless::Vec::new();
                let mut s: String<64> = String::new();
                let _ = s.push_str("[PREV]");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 0,
                    s,
                });
                cmds
            }

            // Tilt → → placeholder NEXT
            InputEvent::EncoderTilt {
                dir: Direction::Right,
                edge: Edge::Press,
            } => {
                let mut cmds: heapless::Vec<RenderCommand, 16> = heapless::Vec::new();
                let mut s: String<64> = String::new();
                let _ = s.push_str("[NEXT]");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 0,
                    s,
                });
                cmds
            }

            // Encoder rotation → scroll selection
            InputEvent::EncoderDelta(delta) => {
                if delta > 0 {
                    self.selection = self.selection.saturating_add(delta.unsigned_abs() as usize);
                } else {
                    self.selection = self.selection.saturating_sub(delta.unsigned_abs() as usize);
                }
                self.render()
            }

            // Center push → SELECT / enter
            InputEvent::Button {
                row: 0,
                col: 0,
                edge: Edge::Press,
            } => {
                self.select();
                self.render()
            }

            // Release events and all other buttons → no-op
            _ => heapless::Vec::new(),
        }
    }

    /// Handle a SELECT action: enter the currently highlighted menu item.
    fn select(&mut self) {
        if self.screen == Screen::MainMenu {
            self.screen = match self.selection {
                0 => Screen::Queue,
                1 => Screen::Library,
                2 => Screen::NowPlaying,
                _ => Screen::NowPlaying,
            };
            self.selection = 0;
        }
    }

    /// Build the render command list for the current screen state.
    fn render(&self) -> heapless::Vec<RenderCommand, 16> {
        let mut cmds: heapless::Vec<RenderCommand, 16> = heapless::Vec::new();
        let _ = cmds.push(RenderCommand::Clear);

        match self.screen {
            Screen::NowPlaying => {
                let mut s: String<64> = String::new();
                let _ = s.push_str("Now Playing");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 1,
                    s,
                });
            }
            Screen::MainMenu => {
                let items = ["Queue", "Library", "Now Playing"];
                for (i, item) in items.iter().enumerate() {
                    let y = (i as u16) * 16;
                    let mut s: String<64> = String::new();
                    if i == self.selection {
                        let _ = s.push_str("> ");
                    } else {
                        let _ = s.push_str("  ");
                    }
                    let _ = s.push_str(item);
                    let _ = cmds.push(RenderCommand::Text {
                        x: 0,
                        y,
                        font: 0,
                        s,
                    });
                }
            }
            Screen::Queue => {
                let mut s: String<64> = String::new();
                let _ = s.push_str("Queue");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 1,
                    s,
                });
                let mut sel: String<64> = String::new();
                let _ = sel.push_str("Item #");
                let _ = write!(sel, "{}", self.selection);
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 16,
                    font: 0,
                    s: sel,
                });
            }
            Screen::Library => {
                let mut s: String<64> = String::new();
                let _ = s.push_str("Library");
                let _ = cmds.push(RenderCommand::Text {
                    x: 0,
                    y: 0,
                    font: 1,
                    s,
                });
            }
        }

        let _ = cmds.push(RenderCommand::Flip);
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_protocol::{Direction, Edge, InputEvent, RenderCommand};
    use pretty_assertions::assert_eq;

    fn text_content(cmd: &RenderCommand) -> Option<&str> {
        if let RenderCommand::Text { s, .. } = cmd {
            Some(s.as_str())
        } else {
            None
        }
    }

    #[test]
    fn initial_screen_is_now_playing() {
        let ui = UiState::new();
        assert_eq!(ui.screen, Screen::NowPlaying);
        assert_eq!(ui.selection, 0);
    }

    #[test]
    fn tilt_up_transitions_to_main_menu() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        assert_eq!(ui.screen, Screen::MainMenu);
        // Should contain text items
        let texts: Vec<&str> = cmds.iter().filter_map(text_content).collect();
        assert!(texts.iter().any(|t| t.contains("Queue")));
    }

    #[test]
    fn tilt_down_emits_play_pause_text() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Down,
            edge: Edge::Press,
        });
        let texts: Vec<&str> = cmds.iter().filter_map(text_content).collect();
        assert!(texts.iter().any(|t| t.contains("PLAY/PAUSE")));
    }

    #[test]
    fn tilt_left_emits_prev_text() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Left,
            edge: Edge::Press,
        });
        let texts: Vec<&str> = cmds.iter().filter_map(text_content).collect();
        assert!(texts.iter().any(|t| t.contains("PREV")));
    }

    #[test]
    fn tilt_right_emits_next_text() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Right,
            edge: Edge::Press,
        });
        let texts: Vec<&str> = cmds.iter().filter_map(text_content).collect();
        assert!(texts.iter().any(|t| t.contains("NEXT")));
    }

    #[test]
    fn encoder_delta_positive_increases_selection() {
        let mut ui = UiState::new();
        ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        assert_eq!(ui.selection, 0);
        ui.handle(InputEvent::EncoderDelta(2));
        assert_eq!(ui.selection, 2);
    }

    #[test]
    fn encoder_delta_negative_decreases_selection() {
        let mut ui = UiState::new();
        ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        ui.handle(InputEvent::EncoderDelta(3));
        ui.handle(InputEvent::EncoderDelta(-1));
        assert_eq!(ui.selection, 2);
    }

    #[test]
    fn encoder_delta_saturates_at_zero() {
        let mut ui = UiState::new();
        ui.handle(InputEvent::EncoderDelta(-5));
        assert_eq!(ui.selection, 0);
    }

    #[test]
    fn center_push_selects_queue_when_on_first_menu_item() {
        let mut ui = UiState::new();
        // Go to main menu, selection = 0 (Queue)
        ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        ui.handle(InputEvent::Button {
            row: 0,
            col: 0,
            edge: Edge::Press,
        });
        assert_eq!(ui.screen, Screen::Queue);
    }

    #[test]
    fn center_push_selects_library_when_on_second_item() {
        let mut ui = UiState::new();
        ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        ui.handle(InputEvent::EncoderDelta(1));
        ui.handle(InputEvent::Button {
            row: 0,
            col: 0,
            edge: Edge::Press,
        });
        assert_eq!(ui.screen, Screen::Library);
    }

    #[test]
    fn render_output_starts_with_clear_and_ends_with_flip() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        assert_eq!(cmds.first(), Some(&RenderCommand::Clear));
        assert_eq!(cmds.last(), Some(&RenderCommand::Flip));
    }

    #[test]
    fn release_events_produce_no_commands() {
        let mut ui = UiState::new();
        let cmds = ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Release,
        });
        assert!(cmds.is_empty());
    }

    #[test]
    fn queue_renders_item_number_above_9() {
        let mut ui = UiState::new();
        // Navigate to Queue screen
        ui.handle(InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        });
        ui.handle(InputEvent::Button {
            row: 0,
            col: 0,
            edge: Edge::Press,
        });
        assert_eq!(ui.screen, Screen::Queue);
        // Scroll to item 12 — render returns Queue screen commands
        let cmds = ui.handle(InputEvent::EncoderDelta(12));
        let texts: Vec<&str> = cmds.iter().filter_map(text_content).collect();
        assert!(
            texts.iter().any(|t| t.contains("12")),
            "expected 'Item #12' in render output, got: {:?}",
            texts
        );
    }

    #[test]
    fn encoder_delta_i8_min_does_not_overflow() {
        let mut ui = UiState::new();
        // Start at a high selection so subtraction is meaningful
        ui.handle(InputEvent::EncoderDelta(100));
        // i8::MIN is -128; unsigned_abs gives 128 — should not panic
        ui.handle(InputEvent::EncoderDelta(i8::MIN));
        // Just verifying no panic; selection saturates at 0
        assert_eq!(ui.selection, 0);
    }
}
