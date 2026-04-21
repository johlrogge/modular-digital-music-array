use heapless::String;
use serde::{Deserialize, Serialize};

/// Cardinal direction for encoder tilt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Button / tilt edge — whether this is a press-down or release event.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Edge {
    Press,
    Release,
}

/// Events sent from panel firmware → 909 service.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// Signed rotation delta since last report (positive = clockwise).
    EncoderDelta(i8),
    /// Multi-directional tilt on the Alps RKJXT1F42001.
    EncoderTilt { dir: Direction, edge: Edge },
    /// Raw matrix position for Choc switch / center push.
    Button { row: u8, col: u8, edge: Edge },
}

/// Render commands sent from 909 service → panel firmware.
/// Command-list format keeps bandwidth low and enables partial updates.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RenderCommand {
    /// Clear the display.
    Clear,
    /// Draw text at pixel position (x, y) with font index and content.
    Text {
        x: u16,
        y: u16,
        font: u8,
        s: String<64>,
    },
    /// Draw a line between two points.
    Line { x0: u16, y0: u16, x1: u16, y1: u16 },
    /// Draw a rectangle, optionally filled.
    Rect {
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        filled: bool,
    },
    /// Invert a rectangular region (Sharp partial-update).
    Invert { x: u16, y: u16, w: u16, h: u16 },
    /// Commit the framebuffer to the display.
    Flip,
}
