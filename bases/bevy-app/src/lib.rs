pub mod config;
pub mod filters;
pub mod ipc;
pub mod playlists;
pub mod results;
pub mod ui;

use bevy::prelude::*;
use bevy_egui::EguiPlugin;

use config::DjWorkspaceConfig;
use filters::FilterState;
use ipc::{poll_ipc_events, setup_ipc};
use playlists::{CentralView, PlaylistState};
use results::SearchResults;
use ui::ui_system;

pub fn run(config: DjWorkspaceConfig) {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "MDMA DJ Workspace".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .insert_resource(config)
        .init_resource::<PlaylistState>()
        .init_resource::<CentralView>()
        .init_resource::<FilterState>()
        .init_resource::<SearchResults>()
        .add_systems(Startup, setup_ipc)
        .add_systems(Update, (poll_ipc_events, ui_system).chain())
        .run();
}
