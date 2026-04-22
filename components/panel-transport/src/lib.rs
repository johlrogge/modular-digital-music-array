//! Host-side (std) abstraction for the USB-CDC link to the panel. Not used in firmware.
//!
//! TODO: real USB-CDC impl — serialport + postcard framing here (deferred until hardware lands)
//!
//! This crate exposes the same public surface as `panel-transport-fake` so that
//! polylith's build-time interface swap works without any Rust trait abstraction.
//! The workspace dep `panel-transport` is pointed at whichever concrete component
//! should ship (`panel-transport-fake` for now, this crate when hardware lands).

use panel_protocol::{InputEvent, RenderCommand};
use thiserror::Error;

/// Errors that the transport may surface.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("send failed: {0}")]
    Send(String),
    #[error("channel closed")]
    Closed,
    #[error("not implemented")]
    NotImplemented,
}

/// The transport end held by the 909 service / panel-host.
pub struct Transport;

/// The control handle paired with a [`Transport`].
pub struct Handle;

/// Create a linked `(Transport, Handle)` pair.
///
/// TODO: this will open the real USB-CDC serial port once hardware lands.
pub fn pair(_buffer: usize) -> (Transport, Handle) {
    (Transport, Handle)
}

impl Transport {
    /// Send a render command to the panel.
    pub async fn send(&mut self, _cmd: RenderCommand) -> Result<(), TransportError> {
        Err(TransportError::NotImplemented)
    }

    /// Receive the next input event from the panel.
    /// Returns `None` when the transport is closed / exhausted.
    pub async fn recv(&mut self) -> Option<InputEvent> {
        None
    }
}

impl Handle {
    /// Push a synthetic input event into the transport.
    pub async fn push_event(&self, _ev: InputEvent) -> Result<(), TransportError> {
        Err(TransportError::NotImplemented)
    }

    /// Receive the next render command that came out of the transport.
    pub async fn next_command(&mut self) -> Option<RenderCommand> {
        None
    }
}
