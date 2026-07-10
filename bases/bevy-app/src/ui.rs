use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use mdma_client::TrackInfo;
use music_primitives::TrackRole;

use crate::filters::{build_track_query, FilterState};
use crate::ipc::{ConnectionStatus, IpcChannels, IpcRequest};
use crate::playlists::{CentralView, PlaylistState};
use crate::results::{
    fmt_bpm, fmt_energy, fmt_key, fmt_role, sort_tracks, SearchResults, SortColumn,
};

/// Column header labels, shared between the playlist and candidates tables.
/// Order matches `SortColumn` variants and both panels' column layout.
fn track_header_labels() -> [&'static str; 6] {
    ["Title", "Artist", "BPM", "Key", "Role", "Energy"]
}

/// Render a single data row for a track inside an `egui::Grid`.
///
/// Missing title/artist display as an en dash, matching the `fmt_*` helpers.
/// Callers must call `ui.end_row()` after this if needed.
fn track_row(ui: &mut egui::Ui, track: &TrackInfo) {
    ui.label(track.title.as_deref().unwrap_or("\u{2013}"));
    ui.label(track.artist.as_deref().unwrap_or("\u{2013}"));
    ui.label(fmt_bpm(&track.bpm));
    ui.label(fmt_key(&track.key));
    ui.label(fmt_role(&track.role));
    ui.label(fmt_energy(&track.energy));
    ui.end_row();
}

/// All `TrackRole` variants in display order, for the combo box.
const ALL_ROLES: [TrackRole; 7] = [
    TrackRole::Opener,
    TrackRole::BuildUp,
    TrackRole::Peak,
    TrackRole::Banger,
    TrackRole::CoolDown,
    TrackRole::Closer,
    TrackRole::Filler,
];

/// Bevy Update system: renders the full DJ Workspace UI.
#[allow(deprecated)]
pub fn ui_system(
    mut contexts: EguiContexts,
    status: Res<ConnectionStatus>,
    mut playlist_state: ResMut<PlaylistState>,
    mut central_view: ResMut<CentralView>,
    mut filter_state: ResMut<FilterState>,
    mut search_results: ResMut<SearchResults>,
    channels: Res<IpcChannels>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let status_text = match status.as_ref() {
        ConnectionStatus::Connecting => "Connecting\u{2026}".to_string(),
        ConnectionStatus::Connected => "Connected".to_string(),
        ConnectionStatus::Failed(msg) => format!("Connection failed: {}", msg),
    };

    // Snapshot playlist names to avoid borrow-checker conflicts while mutating
    // the resource inside the loop body.
    let playlist_names: Vec<String> = playlist_state
        .names
        .iter()
        .map(|n| n.as_str().to_string())
        .collect();
    let current_selected = playlist_state.selected;

    // -------------------------------------------------------------------------
    // Left panel — playlist list + filters
    // -------------------------------------------------------------------------
    egui::SidePanel::left("playlists_panel")
        .min_width(200.0)
        .show(ctx, |ui| {
            ui.heading("Playlists");
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("playlist_scroll")
                .max_height(200.0)
                .show(ui, |ui| {
                    for (idx, name) in playlist_names.iter().enumerate() {
                        let selected = current_selected == Some(idx);
                        if ui
                            .add(egui::SelectableLabel::new(selected, name.as_str()))
                            .clicked()
                            && current_selected != Some(idx)
                        {
                            playlist_state.selected = Some(idx);
                            playlist_state.tracks.clear();
                            playlist_state.loading = true;
                            *central_view = CentralView::Playlist;

                            // Clone the PlaylistName from the resource (names
                            // are still intact — only tracks/selected changed).
                            if let Some(playlist_name) = playlist_state.names.get(idx).cloned() {
                                let _ = channels
                                    .tx
                                    .send(IpcRequest::GetPlaylistTracks(playlist_name));
                            }
                        }
                    }
                });

            ui.separator();
            ui.heading("Filters");
            ui.add_space(4.0);

            // BPM filter
            ui.horizontal(|ui| {
                ui.label("BPM:");
                ui.add(egui::TextEdit::singleline(&mut filter_state.bpm_text).hint_text("128+-3"));
            });

            // Key filter
            ui.horizontal(|ui| {
                ui.label("Key:");
                ui.add(egui::TextEdit::singleline(&mut filter_state.key_text).hint_text("8A~"));
            });

            // Energy filter
            ui.horizontal(|ui| {
                ui.label("Energy:");
                ui.add(egui::TextEdit::singleline(&mut filter_state.energy_text).hint_text("5..8"));
            });

            // Role combo box
            ui.horizontal(|ui| {
                ui.label("Role:");
                let role_label = filter_state
                    .role
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "(none)".to_string());

                egui::ComboBox::from_id_salt("role_combo")
                    .selected_text(role_label)
                    .show_ui(ui, |ui| {
                        // "(none)" option
                        ui.selectable_value(&mut filter_state.role, None, "(none)");
                        for role in ALL_ROLES {
                            ui.selectable_value(
                                &mut filter_state.role,
                                Some(role),
                                role.to_string(),
                            );
                        }
                    });
            });

            ui.add_space(4.0);

            // Search button — disabled while a search is in flight
            let search_btn = ui.add_enabled(!filter_state.searching, egui::Button::new("Search"));

            if search_btn.clicked() {
                match build_track_query(&filter_state) {
                    Err(msg) => {
                        filter_state.error = Some(msg);
                    }
                    Ok(query) => {
                        filter_state.error = None;
                        filter_state.searching = true;
                        let _ = channels.tx.send(IpcRequest::Search(Box::new(query)));
                        *central_view = CentralView::Candidates;
                    }
                }
            }

            // Error label (shown in red when set)
            if let Some(ref err) = filter_state.error.clone() {
                ui.colored_label(egui::Color32::RED, err);
            }
        });

    // -------------------------------------------------------------------------
    // Central panel — track table or candidates
    // -------------------------------------------------------------------------
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.label(&status_text);
        ui.separator();

        match *central_view {
            CentralView::Playlist => {
                show_playlist_panel(ui, &playlist_state);
            }
            CentralView::Candidates => {
                show_candidates_panel(ui, &mut search_results, &filter_state);
            }
        }
    });
}

/// Render the playlist track table inside the central panel.
fn show_playlist_panel(ui: &mut egui::Ui, state: &PlaylistState) {
    match state.selected {
        None => {
            ui.label("Select a playlist.");
        }
        Some(idx) => {
            let name = state.names.get(idx).map(|n| n.as_str()).unwrap_or("?");

            if state.loading {
                ui.label(format!("Loading {name}\u{2026}"));
            } else {
                ui.label(format!(
                    "Playlist: {} ({} tracks)",
                    name,
                    state.tracks.len()
                ));
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("tracks_scroll")
                    .show(ui, |ui| {
                        egui::Grid::new("tracks_grid")
                            .striped(true)
                            .num_columns(6)
                            .show(ui, |ui| {
                                // Header row — plain strong labels (not clickable)
                                for label in track_header_labels() {
                                    ui.strong(label);
                                }
                                ui.end_row();

                                for track in &state.tracks {
                                    track_row(ui, track);
                                }
                            });
                    });
            }
        }
    }
}

/// Render the candidates table with sortable column headers.
fn show_candidates_panel(
    ui: &mut egui::Ui,
    results: &mut SearchResults,
    filter_state: &FilterState,
) {
    if filter_state.searching {
        ui.label("Searching\u{2026}");
        return;
    }

    ui.label(format!("Mix candidates ({})", results.tracks.len()));
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("candidates_scroll")
        .show(ui, |ui| {
            egui::Grid::new("candidates_grid")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    // Sortable header buttons — labels sourced from shared array
                    let current_sort = results.sort;
                    let ascending = results.ascending;

                    let mut clicked_col: Option<SortColumn> = None;

                    let sort_columns = [
                        SortColumn::Title,
                        SortColumn::Artist,
                        SortColumn::Bpm,
                        SortColumn::Key,
                        SortColumn::Role,
                        SortColumn::Energy,
                    ];

                    for (col, label) in sort_columns.iter().zip(track_header_labels()) {
                        let header_text = if current_sort == *col {
                            if ascending {
                                format!("{label} \u{25b2}") // ▲
                            } else {
                                format!("{label} \u{25bc}") // ▼
                            }
                        } else {
                            label.to_string()
                        };

                        if ui.button(header_text).clicked() {
                            clicked_col = Some(*col);
                        }
                    }
                    ui.end_row();

                    // Apply sort change if a header was clicked.
                    if let Some(col) = clicked_col {
                        if col == results.sort {
                            // Toggle direction.
                            results.ascending = !results.ascending;
                        } else {
                            results.sort = col;
                            results.ascending = true;
                        }
                        sort_tracks(&mut results.tracks, results.sort, results.ascending);
                    }

                    // Data rows
                    for track in &results.tracks {
                        track_row(ui, track);
                    }
                });
        });
}
