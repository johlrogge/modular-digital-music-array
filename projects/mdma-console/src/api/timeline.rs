//! The timeline of one track for the workbench's previewing row: its cues and its bar lines.
//!
//! The bar lines come from `music_primitives::BeatGrid` (through `BeatGridInfo::grid`), the one
//! home of beat arithmetic; the browser only draws what it is given.

use super::{json_or_failure, lib};
use crate::AppState;
use axum::{
    extract::{Path, State},
    response::Response,
};
use library_ipc_client::{ContentHash, CueInfo, CueKind, TrackInfo};
use serde::Serialize;
use std::sync::Arc;

/// What kind of cue it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiCueKind {
    Memory,
    Hot,
    Loop,
}

/// One cue: where it is, what kind, and its label and pad index when it has them (hot cues; pads
/// count from 0).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiCue {
    pub at_ms: u32,
    pub kind: ApiCueKind,
    pub label: Option<String>,
    pub index: Option<u8>,
}

impl From<&CueInfo> for ApiCue {
    fn from(cue: &CueInfo) -> Self {
        Self {
            at_ms: cue.position_ms,
            kind: match cue.kind {
                CueKind::Memory => ApiCueKind::Memory,
                CueKind::Hot => ApiCueKind::Hot,
                CueKind::Loop { .. } => ApiCueKind::Loop,
            },
            label: cue.label.clone(),
            index: cue.index,
        }
    }
}

/// The beat grid with its bar lines worked out (`bars_ms`: every downbeat within the track).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiGrid {
    pub bpm: f32,
    pub beats_per_bar: u8,
    pub first_beat_ms: u32,
    pub bars_ms: Vec<u64>,
}

/// The body of `GET /api/tracks/{hash}/timeline`. `grid` is null for a track with no beat grid;
/// `duration_ms` is null when the library does not know how long the track is (then there are no
/// bar lines either).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiTimeline {
    pub duration_ms: Option<u64>,
    pub cues: Vec<ApiCue>,
    pub grid: Option<ApiGrid>,
}

impl From<&TrackInfo> for ApiTimeline {
    fn from(t: &TrackInfo) -> Self {
        let duration_ms = t.duration.map(|d| u64::from(d.value()) * 1000);
        let grid = t
            .beat_grid
            .as_ref()
            .and_then(|info| info.grid())
            .map(|grid| ApiGrid {
                bpm: grid.bpm().as_f32(),
                beats_per_bar: grid.beats_per_bar().get(),
                first_beat_ms: grid.first_beat_ms(),
                bars_ms: duration_ms.map(|d| grid.bars(d)).unwrap_or_default(),
            });
        Self {
            duration_ms,
            cues: t.memory_cues.iter().map(ApiCue::from).collect(),
            grid,
        }
    }
}

/// `GET /api/tracks/{hash}/timeline`: `{duration_ms, cues: [{at_ms, kind, label, index}], grid:
/// null | {bpm, beats_per_bar, first_beat_ms, bars_ms}}`. A library failure answers by
/// [`super::library_failure_status`] (a hash the library does not know is a 422).
pub async fn timeline(State(state): State<Arc<AppState>>, Path(hash): Path<String>) -> Response {
    json_or_failure(
        lib(&state, move |backend| {
            backend
                .get_track(&ContentHash::new(hash))
                .map(|track| ApiTimeline::from(&track))
        })
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        bare_track, body_json, library_ok, send_get, test_app_with, track_not_found, FakeGateway,
    };
    use axum::http::StatusCode;
    use gateway_client::{GatewayRequest, GatewayResponse, LibraryRequest, LibraryResponse};
    use library_ipc_client::{BeatGridInfo, Bpm, DurationSeconds};
    use serde_json::json;

    /// A 5 s track at 120 BPM in 4/4 with the first beat at 100 ms: a beat every 500 ms, a bar
    /// every 2000 ms. Cues in storage order, not time order.
    fn gridded() -> TrackInfo {
        let mut t = bare_track("sha256:aa");
        t.duration = Some(DurationSeconds::new(5));
        t.beat_grid = Some(BeatGridInfo {
            first_beat_ms: 100,
            bpm: Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        });
        t.memory_cues = vec![
            CueInfo {
                position_ms: 3000,
                kind: CueKind::Hot,
                label: Some("Drop".into()),
                index: Some(0),
            },
            CueInfo {
                position_ms: 1000,
                kind: CueKind::Memory,
                label: None,
                index: None,
            },
        ];
        t
    }

    fn serving(track: TrackInfo) -> FakeGateway {
        FakeGateway::start(move |req| match req {
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } if hash.as_str() == "sha256:aa" => {
                library_ok(LibraryResponse::Track(Box::new(track.clone())))
            }
            GatewayRequest::Library {
                request: LibraryRequest::GetTrack { hash },
            } => track_not_found(hash.as_str()),
            other => GatewayResponse::Error {
                message: format!("unexpected {other:?}"),
            },
        })
    }

    #[tokio::test]
    async fn timeline_has_the_cues_and_the_bars_of_the_grid() {
        let gw = serving(gridded());
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/sha256:aa/timeline",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await,
            json!({
                "duration_ms": 5000,
                "cues": [
                    {"at_ms": 3000, "kind": "hot", "label": "Drop", "index": 0},
                    {"at_ms": 1000, "kind": "memory", "label": null, "index": null},
                ],
                "grid": {
                    "bpm": 120.0,
                    "beats_per_bar": 4,
                    "first_beat_ms": 100,
                    "bars_ms": [100, 2100, 4100],
                },
            })
        );
    }

    #[tokio::test]
    async fn timeline_without_a_beat_grid_has_a_null_grid() {
        let mut t = gridded();
        t.beat_grid = None;
        let gw = serving(t);
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/sha256:aa/timeline",
            None,
        )
        .await;
        assert!(body_json(res).await["grid"].is_null());
    }

    #[tokio::test]
    async fn timeline_of_an_unknown_hash_follows_the_library_policy() {
        let gw = serving(gridded());
        let res = send_get(
            test_app_with(gw.addr()),
            "/api/tracks/sha256:nope/timeline",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn a_track_of_unknown_length_has_no_bar_lines() {
        let mut t = gridded();
        t.duration = None;
        let timeline = ApiTimeline::from(&t);
        assert_eq!(timeline.duration_ms, None);
        assert_eq!(timeline.grid.unwrap().bars_ms, Vec::<u64>::new());
    }

    #[test]
    fn a_grid_of_zero_beats_per_bar_is_no_grid() {
        let mut t = gridded();
        t.beat_grid.as_mut().unwrap().beats_per_bar = 0;
        assert_eq!(ApiTimeline::from(&t).grid, None);
    }
}
