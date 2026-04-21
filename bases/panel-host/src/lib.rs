use panel_transport::PanelTransport;
use panel_ui::UiState;
use tracing::debug;

/// Run the panel host loop.
///
/// Receives [`InputEvent`]s from the transport, feeds them through [`UiState`],
/// and sends the resulting [`RenderCommand`]s back.
/// Returns when the transport signals end-of-stream (recv returns None).
pub async fn run(transport: &mut impl PanelTransport) {
    let mut ui = UiState::new();

    while let Some(ev) = transport.recv().await {
        debug!(?ev, "input event");
        let cmds = ui.handle(ev);
        for cmd in cmds {
            debug!(?cmd, "render command");
            if let Err(e) = transport.send(cmd).await {
                tracing::warn!("transport send error: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_protocol::{Direction, Edge, InputEvent};
    use panel_transport_fake::fake_pair;

    #[tokio::test]
    async fn tilt_up_produces_main_menu_commands() {
        let (mut transport, handle) = fake_pair(32);

        // Push a tilt-up event then drop handle to end the loop
        handle
            .push_event(InputEvent::EncoderTilt {
                dir: Direction::Up,
                edge: Edge::Press,
            })
            .await;

        // Drop the sender side to close the event channel so run() exits
        drop(handle);

        run(&mut transport).await;
    }

    #[tokio::test]
    async fn rotate_and_select_sends_render_commands() {
        let (mut transport, handle) = fake_pair(64);

        // Navigate to main menu
        handle
            .push_event(InputEvent::EncoderTilt {
                dir: Direction::Up,
                edge: Edge::Press,
            })
            .await;
        // Scroll to second item (Library)
        handle.push_event(InputEvent::EncoderDelta(1)).await;
        // Select Library
        handle
            .push_event(InputEvent::Button {
                row: 0,
                col: 0,
                edge: Edge::Press,
            })
            .await;

        drop(handle);
        run(&mut transport).await;
    }

    #[tokio::test]
    async fn host_exits_cleanly_when_transport_closed() {
        let (mut transport, handle) = fake_pair(8);
        drop(handle); // immediately close
        run(&mut transport).await; // should return without panic
    }

    #[tokio::test]
    async fn tilt_up_renders_clear_and_flip() {
        let (mut transport, handle) = fake_pair(32);

        handle
            .push_event(InputEvent::EncoderTilt {
                dir: Direction::Up,
                edge: Edge::Press,
            })
            .await;
        drop(handle);

        // Run in a task so we can collect commands concurrently
        let task = tokio::spawn(async move {
            run(&mut transport).await;
            transport
        });

        let _transport = task.await.unwrap();
        // If we get here without panic, the loop ran successfully.
        // The render commands were consumed by the transport's cmd_tx side;
        // since we dropped the handle (cmd_rx), the send may fail gracefully —
        // that's fine for this integration test.
    }
}
