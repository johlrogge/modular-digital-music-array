use panel_protocol::{InputEvent, RenderCommand};
use panel_transport::{PanelTransport, TransportError};
use tokio::sync::mpsc;

/// The transport end held by the 909 service / panel-host.
/// Implements [`PanelTransport`]: send commands, receive events.
pub struct FakeTransport {
    event_rx: mpsc::Receiver<InputEvent>,
    cmd_tx: mpsc::Sender<RenderCommand>,
}

/// The test-control handle for the fake transport pair.
///
/// Use this to push synthetic [`InputEvent`]s into the transport and
/// inspect the [`RenderCommand`]s that come out.
pub struct FakeHandle {
    event_tx: mpsc::Sender<InputEvent>,
    cmd_rx: mpsc::Receiver<RenderCommand>,
}

/// Create a linked `(FakeTransport, FakeHandle)` pair backed by tokio channels.
///
/// ```rust
/// let (transport, handle) = panel_transport_fake::fake_pair(32);
/// ```
pub fn fake_pair(buffer: usize) -> (FakeTransport, FakeHandle) {
    let (event_tx, event_rx) = mpsc::channel(buffer);
    let (cmd_tx, cmd_rx) = mpsc::channel(buffer);
    (
        FakeTransport { event_rx, cmd_tx },
        FakeHandle { event_tx, cmd_rx },
    )
}

impl FakeHandle {
    /// Push a synthetic input event into the transport.
    pub async fn push_event(&self, ev: InputEvent) {
        self.event_tx.send(ev).await.expect("transport closed");
    }

    /// Receive the next render command that came out of the transport.
    /// Returns `None` if the transport side is dropped.
    pub async fn next_command(&mut self) -> Option<RenderCommand> {
        self.cmd_rx.recv().await
    }
}

impl PanelTransport for FakeTransport {
    async fn send(&mut self, cmd: RenderCommand) -> Result<(), TransportError> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    async fn recv(&mut self) -> Option<InputEvent> {
        self.event_rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_protocol::{Direction, Edge, InputEvent, RenderCommand};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn push_event_received_by_transport() {
        let (mut transport, handle) = fake_pair(8);
        let ev = InputEvent::EncoderDelta(3);
        handle.push_event(ev.clone()).await;
        let received = transport.recv().await.expect("expected event");
        assert_eq!(received, ev);
    }

    #[tokio::test]
    async fn send_command_received_by_handle() {
        let (mut transport, mut handle) = fake_pair(8);
        let cmd = RenderCommand::Flip;
        transport.send(cmd.clone()).await.expect("send failed");
        let received = handle.next_command().await.expect("expected command");
        assert_eq!(received, cmd);
    }

    #[tokio::test]
    async fn multiple_events_in_order() {
        let (mut transport, handle) = fake_pair(8);
        let events = vec![
            InputEvent::EncoderDelta(1),
            InputEvent::EncoderTilt {
                dir: Direction::Up,
                edge: Edge::Press,
            },
            InputEvent::Button {
                row: 0,
                col: 0,
                edge: Edge::Press,
            },
        ];
        for ev in &events {
            handle.push_event(ev.clone()).await;
        }
        for expected in &events {
            let got = transport.recv().await.expect("expected event");
            assert_eq!(&got, expected);
        }
    }

    #[tokio::test]
    async fn multiple_commands_in_order() {
        let (mut transport, mut handle) = fake_pair(8);
        let cmds = vec![RenderCommand::Clear, RenderCommand::Flip];
        for cmd in &cmds {
            transport.send(cmd.clone()).await.expect("send failed");
        }
        for expected in &cmds {
            let got = handle.next_command().await.expect("expected command");
            assert_eq!(&got, expected);
        }
    }

    #[tokio::test]
    async fn recv_returns_none_when_handle_dropped() {
        let (mut transport, handle) = fake_pair(8);
        drop(handle);
        assert!(transport.recv().await.is_none());
    }
}
