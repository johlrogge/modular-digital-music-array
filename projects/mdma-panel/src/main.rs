use color_eyre::eyre::Result;
use panel_protocol::{Direction, Edge, InputEvent};
use panel_transport::pair;
use std::time::Duration;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mdma_panel=debug,panel_host=debug".into()),
        )
        .init();

    info!("mdma-panel starting (fake transport)");

    let (mut transport, mut handle) = pair(64);

    // Spawn the panel-host loop
    let host_task = tokio::spawn(async move {
        panel_host::run(&mut transport).await;
    });

    // Canned event sequence simulating user interaction
    let canned_events: &[InputEvent] = &[
        // Rotate: scroll by 2 ticks
        InputEvent::EncoderDelta(1),
        InputEvent::EncoderDelta(1),
        // Tilt up: go to main menu
        InputEvent::EncoderTilt {
            dir: Direction::Up,
            edge: Edge::Press,
        },
        // Rotate to second menu item
        InputEvent::EncoderDelta(1),
        // Center push: select Library
        InputEvent::Button {
            row: 0,
            col: 0,
            edge: Edge::Press,
        },
    ];

    // Fire events with 500ms pauses, logging render commands as they arrive
    for ev in canned_events {
        info!(?ev, "sending input event");
        handle.push_event(ev.clone()).await.expect("push failed");
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Drain any pending render commands
        while let Ok(cmd) =
            tokio::time::timeout(Duration::from_millis(50), handle.next_command()).await
        {
            match cmd {
                Some(c) => info!(?c, "render command received"),
                None => break,
            }
        }
    }

    // Signal end-of-stream so the host loop exits
    drop(handle);

    host_task.await?;

    info!("mdma-panel done");
    Ok(())
}
