use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin};

pub fn run() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "MDMA DJ Workspace".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .add_systems(Update, ui_system)
        .run();
}

fn ui_system(mut contexts: EguiContexts) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    #[allow(deprecated)]
    egui::SidePanel::left("filters_panel")
        .min_width(200.0)
        .show(ctx, |ui| {
            ui.heading("Filters");
            ui.separator();
            ui.label("(placeholder)");
        });

    #[allow(deprecated)]
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("DJ Workspace");
        ui.label("(placeholder)");
    });
}
