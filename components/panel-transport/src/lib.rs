//! Host-side (std) abstraction for the USB-CDC link to the panel. Not used in firmware.
//!
//! TODO: serialport + postcard framing here (real USB-CDC transport deferred until hardware lands)
//!
//! This module defines the [`PanelTransport`] trait only.  The fake implementation
//! lives in `panel-transport-fake` and is used during testing and simulation.

use panel_protocol::{InputEvent, RenderCommand};
use thiserror::Error;

/// Errors that a transport implementation may surface.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("send failed: {0}")]
    Send(String),
    #[error("channel closed")]
    Closed,
}

/// Bidirectional async transport between the 909 service and the panel.
///
/// Implementations live in separate crates (`panel-transport-fake` for tests,
/// a real serialport-backed impl to be added when hardware lands).
#[allow(async_fn_in_trait)]
pub trait PanelTransport {
    /// Send a render command to the panel.
    async fn send(&mut self, cmd: RenderCommand) -> Result<(), TransportError>;

    /// Receive the next input event from the panel.
    /// Returns `None` when the transport is closed / exhausted.
    async fn recv(&mut self) -> Option<InputEvent>;
}
