//! DJ Workspace Bevy application base.
//!
//! **Logging contract:** this base disables Bevy's `LogPlugin`; any binary
//! using it must install its own tracing subscriber before calling `run()`,
//! otherwise logs are silently lost.

pub mod config;
pub mod filters;
pub mod ipc;
pub mod playlists;
pub mod results;
pub mod ui;

use bevy::prelude::*;
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};

use config::DjWorkspaceConfig;
use filters::FilterState;
use ipc::{poll_ipc_events, setup_ipc};
use playlists::{CentralView, PlaylistState};
use results::SearchResults;
use ui::ui_system;

/// Spawns the 2D camera that bevy_egui uses as the anchor for its primary context.
///
/// bevy_egui 0.40 attaches `PrimaryEguiContext` to the first `Camera` it sees
/// (via `setup_primary_egui_context_system` watching `Added<Camera>`).
/// Without a camera the `EguiContext` is never created and `ctx_mut()` always
/// returns `Err(NoEntities)`, causing a blank window.
fn spawn_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

pub fn run(config: DjWorkspaceConfig) {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "MDMA DJ Workspace".into(),
                        ..default()
                    }),
                    ..default()
                })
                // The binary (projects/dj-workspace/src/main.rs) installs its own
                // tracing-subscriber before calling run().  Bevy's LogPlugin would
                // attempt a second global subscriber installation and fail with:
                //   ERROR Could not set global logger … already set.
                // Disable it here so we keep our own stderr subscriber.
                .disable::<bevy::log::LogPlugin>(),
        )
        .add_plugins(EguiPlugin::default())
        .insert_resource(config)
        .init_resource::<PlaylistState>()
        .init_resource::<CentralView>()
        .init_resource::<FilterState>()
        .init_resource::<SearchResults>()
        .add_systems(Startup, (spawn_camera, setup_ipc))
        .add_systems(Update, poll_ipc_events)
        // bevy_egui 0.40 multipass mode: UI systems MUST run in
        // EguiPrimaryContextPass (not Update) or they render nothing.
        .add_systems(EguiPrimaryContextPass, ui_system)
        .run();
}
