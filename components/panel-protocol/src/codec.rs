//! Binary codec: postcard serialization with COBS framing.
//!
//! Both encode and decode produce/consume COBS frames where 0x00 is the
//! frame delimiter.  The encode output includes the trailing 0x00;
//! decode expects the frame *without* the trailing delimiter.

use core::fmt;
use serde::{Deserialize, Serialize};

/// Error returned by [`encode`].
#[derive(Debug)]
pub enum EncodeError {
    Postcard(postcard::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::Postcard(e) => write!(f, "postcard encode error: {e}"),
        }
    }
}

impl From<postcard::Error> for EncodeError {
    fn from(e: postcard::Error) -> Self {
        EncodeError::Postcard(e)
    }
}

/// Error returned by [`decode`].
#[derive(Debug)]
pub enum DecodeError {
    Postcard(postcard::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Postcard(e) => write!(f, "postcard decode error: {e}"),
        }
    }
}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        DecodeError::Postcard(e)
    }
}

/// Serialize `v` into `out` using postcard + COBS framing.
///
/// Returns the number of bytes written (including the trailing 0x00 delimiter).
/// The frame is self-delimiting: the receiver reads until it sees 0x00.
pub fn encode<T: Serialize>(v: &T, out: &mut [u8]) -> Result<usize, EncodeError> {
    // postcard::to_slice_cobs encodes directly into `out` with COBS framing
    // and appends the 0x00 delimiter.
    let written = postcard::to_slice_cobs(v, out).map_err(EncodeError::Postcard)?;
    Ok(written.len())
}

/// Deserialize a COBS-framed slice back into `T`.
///
/// The caller should pass the raw frame bytes **including** the trailing 0x00
/// delimiter (or the exact bytes that `encode` wrote).
pub fn decode<T: for<'de> Deserialize<'de>>(frame: &mut [u8]) -> Result<T, DecodeError> {
    postcard::from_bytes_cobs(frame).map_err(DecodeError::Postcard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Edge, InputEvent, RenderCommand};
    use heapless::String;
    use pretty_assertions::assert_eq;

    fn round_trip_input(ev: InputEvent) -> InputEvent {
        let mut buf = [0u8; 128];
        let n = encode(&ev, &mut buf).expect("encode failed");
        decode(&mut buf[..n]).expect("decode failed")
    }

    fn round_trip_render(cmd: RenderCommand) -> RenderCommand {
        let mut buf = [0u8; 256];
        let n = encode(&cmd, &mut buf).expect("encode failed");
        decode(&mut buf[..n]).expect("decode failed")
    }

    #[test]
    fn encoder_delta_round_trip() {
        let ev = InputEvent::EncoderDelta(5);
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn encoder_delta_negative_round_trip() {
        let ev = InputEvent::EncoderDelta(-3);
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn encoder_tilt_up_press_round_trip() {
        let ev = InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn encoder_tilt_down_release_round_trip() {
        let ev = InputEvent::EncoderTilt {
            dir: Direction::Down,
            edge: Edge::Release,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn encoder_tilt_left_round_trip() {
        let ev = InputEvent::EncoderTilt {
            dir: Direction::Left,
            edge: Edge::Press,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn encoder_tilt_right_round_trip() {
        let ev = InputEvent::EncoderTilt {
            dir: Direction::Right,
            edge: Edge::Release,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn button_press_round_trip() {
        let ev = InputEvent::Button {
            row: 1,
            col: 3,
            edge: Edge::Press,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn button_release_round_trip() {
        let ev = InputEvent::Button {
            row: 0,
            col: 0,
            edge: Edge::Release,
        };
        assert_eq!(round_trip_input(ev.clone()), ev);
    }

    #[test]
    fn render_clear_round_trip() {
        let cmd = RenderCommand::Clear;
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_text_round_trip() {
        let mut s: String<64> = String::new();
        s.push_str("Hello MDMA").unwrap();
        let cmd = RenderCommand::Text {
            x: 10,
            y: 20,
            font: 1,
            s,
        };
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_line_round_trip() {
        let cmd = RenderCommand::Line {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 50,
        };
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_rect_filled_round_trip() {
        let cmd = RenderCommand::Rect {
            x: 5,
            y: 5,
            w: 50,
            h: 20,
            filled: true,
        };
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_rect_unfilled_round_trip() {
        let cmd = RenderCommand::Rect {
            x: 0,
            y: 0,
            w: 320,
            h: 240,
            filled: false,
        };
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_invert_round_trip() {
        let cmd = RenderCommand::Invert {
            x: 0,
            y: 0,
            w: 128,
            h: 16,
        };
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }

    #[test]
    fn render_flip_round_trip() {
        let cmd = RenderCommand::Flip;
        assert_eq!(round_trip_render(cmd.clone()), cmd);
    }
}
