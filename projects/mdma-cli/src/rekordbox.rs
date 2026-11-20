use crate::args::{
    DryRun, ExistingFileArg, OutputDir, PlaylistArg, SanitizedPlaylistName, StdinDash,
};
use crate::ctx::Ctx;
use crate::output::library_error;
use crate::rekordbox_exchange;
use crate::rekordbox_report::{self, Candidate, Problem};
use crate::stdin::{require_stdin_hashes_or_usage, resolve_tracks_or_abort};
use crate::transfer::{
    build_http_client, download_tracks, existing_location_set, export_dest_path_checked,
    export_extension, path_cost, DownloadOutcome, ExportFormat, MAX_LOCATION_URI_BYTES,
};
use bpaf::{construct, Parser};
use client_facts::Facts;
use color_eyre::eyre::{bail, eyre};
use color_eyre::Result;
use hicli::Arg;
use library_ipc_client::{track_hashes, unresolved_entries};
use library_ipc_client::{ContentHash, Key, PlaylistEntry, TrackInfo};
use music_facts::MusicValue;
use playlist_import::{format_write_outcome, write_playlist, LibraryTrackLookup};
use rekordbox_roundtrip::Stars;
use rekordbox_xml::{parse_xml, RekordboxTrack};
use track_matcher::{CandidateTrack, MatchResult};

/// MDMA playlist to export; repeat for several (or give `-` instead to read hashes from stdin)
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "playlist", metavar = "PLAYLIST")]
pub(crate) struct ExportPlaylists(pub Vec<PlaylistArg>);

/// Where `rekordbox export` takes its tracks from: one or more playlists, or the
/// hashes on stdin. Exactly one of the two, decided at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportSource {
    /// At least one playlist.
    Playlists(Vec<PlaylistArg>),
    /// `-`: hashes read from stdin.
    Stdin,
}

impl Arg for ExportSource {
    /// `--playlist PLAYLIST ...` or `-`.
    fn parser() -> impl Parser<Self> {
        let playlists = <ExportPlaylists as Arg>::parser().parse(|ExportPlaylists(playlists)| {
            if playlists.is_empty() {
                Err("expected --playlist PLAYLIST or `-` (read hashes from stdin)")
            } else {
                Ok(Self::Playlists(playlists))
            }
        });
        let stdin = <StdinDash as Arg>::parser().map(|_| Self::Stdin);
        construct!([playlists, stdin])
    }
}

/// Force a fresh XML and re-download everything (discards incremental sync). Also skips
/// grandfathering of existing paths: tracks whose paths exceed the rekordbox Location
/// budget get new, shortened paths (rekordbox will see them as new tracks and cues are lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "replace")]
pub(crate) struct ReplaceXml(pub bool);

// Values of `rekordbox export`; playlist names are validated at parse time.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
pub(crate) struct RekordboxExportArgs {
    /// Directory to write to (created if it doesn't exist; default: ~/Music/mdma_rekordbox)
    pub(crate) output: OutputDir,
    pub(crate) format: Option<ExportFormat>,
    pub(crate) replace: ReplaceXml,
    pub(crate) source: ExportSource,
}

/// Create an MDMA playlist with the matched tracks. Invalid characters (spaces,
/// special chars) are replaced with `-`.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "playlist", metavar = "PLAYLIST")]
pub(crate) struct ImportPlaylist(pub Option<SanitizedPlaylistName>);

/// Import all playlists found in the XML as MDMA playlists
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "all-playlists")]
pub(crate) struct AllPlaylists(pub bool);

/// Update BPM and Key facts from Rekordbox data for matched tracks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arg)]
#[arg(long = "enrich")]
pub(crate) struct Enrich(pub bool);

/// Write a TSV report of the tracks that did not match (unmatched and ambiguous, with
/// the reason and the candidates) and of the playlist entries dropped for them.
/// Also written with --dry-run.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
#[arg(long = "report", metavar = "FILE", complete = files)]
pub(crate) struct ReportFile(pub Option<std::path::PathBuf>);

// Values of `rekordbox import`; the file must exist and the playlist name is
// sanitised, both at parse time. The file is a positional, so it comes last.
#[derive(Debug, Clone, PartialEq, Eq, Arg)]
pub(crate) struct RekordboxImportArgs {
    pub(crate) playlist: ImportPlaylist,
    pub(crate) all_playlists: AllPlaylists,
    pub(crate) enrich: Enrich,
    pub(crate) report: ReportFile,
    pub(crate) dry_run: DryRun,
    pub(crate) path: ExistingFileArg,
}

/// The key of a rekordbox `Tonality`. rekordbox writes Camelot (`8A`) as well as
/// traditional notation (`Abm`, `C`, `F#m`, `Bb`); `Key`'s parser takes both.
fn parse_tonality(tonality: &str) -> Option<Key> {
    tonality.parse::<Key>().ok()
}

/// The `BitRate` (kbps) and `SampleRate` (Hz) the export XML states for a track.
///
/// They must describe the exported file: after an AIFF/WAV transcode the source's
/// facts (FLAC's compressed kbps, a 96 kHz rate that was downsampled) are wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExportedAudio {
    bitrate: Option<u32>,
    sample_rate: Option<u32>,
}

impl ExportedAudio {
    /// The values of the source file, right for an untouched (`Original`) export.
    fn of_source(track: &TrackInfo) -> Self {
        Self {
            bitrate: track.audio.bitrate.map(|b| b.value()),
            sample_rate: track.audio.sample_rate.map(|s| s.value()),
        }
    }

    /// Uncompressed PCM of the given format; the bit rate is computed the way
    /// rekordbox does (see [`audio_transcoder::pcm_bitrate_kbps`]).
    fn of_pcm(sample_rate: u32, bit_depth: u8, channels: u16) -> Self {
        Self {
            bitrate: Some(audio_transcoder::pcm_bitrate_kbps(
                sample_rate,
                bit_depth,
                channels,
            )),
            sample_rate: Some(sample_rate),
        }
    }

    /// Read the real format back from an exported AIFF/WAV file. When it cannot be
    /// read, the values are omitted: the source's would be wrong.
    fn of_exported_pcm_file(path: &std::path::Path) -> Self {
        match audio_metadata::extract_metadata(path) {
            Ok(audio_metadata::TrackMetadata {
                sample_rate: Some(rate),
                bit_depth: Some(depth),
                channels: Some(channels),
                ..
            }) => Self::of_pcm(rate, depth, channels),
            _ => {
                eprintln!(
                    "warning: cannot read the audio format of {}; BitRate and SampleRate are omitted from the XML",
                    path.display()
                );
                Self {
                    bitrate: None,
                    sample_rate: None,
                }
            }
        }
    }

    /// The values for a track exported under `format`: read back from the file
    /// when the destination is a PCM container (AIFF/WAV, whether transcoded or an
    /// already-PCM source served unchanged), the source's otherwise.
    fn of_export(format: &ExportFormat, path: &std::path::Path, track: &TrackInfo) -> Self {
        if format.static_extension().is_some() {
            Self::of_exported_pcm_file(path)
        } else {
            Self::of_source(track)
        }
    }
}

/// Build a single `RekordboxTrack` from MDMA metadata and file facts.
///
/// `dest_path` must be the already-canonicalised destination used for planning —
/// do not re-canonicalise here so the Location URI stays stable.
///
/// Cue points are sourced from `track.memory_cues` (the already-projected
/// structured data from the library), not from raw `get_facts` display strings.
/// This ensures retracted cues are never exported and no duplicate cues appear.
fn build_rekordbox_track(
    track: &TrackInfo,
    dest_path: &std::path::Path,
    ext: &str,
    size: u64,
    facts: &[(String, String)],
    audio: &ExportedAudio,
) -> rekordbox_xml::RefreshedTrack {
    let find_fact = |name: &str| -> Option<String> {
        facts
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let ExportedAudio {
        bitrate,
        sample_rate,
    } = *audio;
    let year = find_fact("Year").or_else(|| find_fact("RecordingYear"));
    let label = find_fact("Label");
    let comment = find_fact("Comment");
    let genre = find_fact("MainGenre")
        .or_else(|| find_fact("FullGenre"))
        .unwrap_or_default();

    let date_added = track
        .added
        .as_deref()
        .and_then(|s| s.split('T').next())
        .map(String::from);

    let tonality = track.key.as_ref().map(rekordbox_xml::key_to_tonality);
    let average_bpm = track.bpm.map(|b| b.as_f32());
    let location = rekordbox_xml::path_to_file_uri(dest_path);

    // Convert the projected CueInfo structs → MusicValue::MemoryCue → PositionMark.
    // Using the projected data avoids the get_facts display-string path (B1 fix).
    let cues_as_music_values: Vec<MusicValue> = track
        .memory_cues
        .iter()
        .map(|c| MusicValue::MemoryCue {
            position_ms: c.position_ms,
            kind: c.kind.clone(),
            label: c.label.clone(),
            index: c.index,
        })
        .collect();
    let position_marks = rekordbox_roundtrip::position_marks_from_cues(&cues_as_music_values);

    rekordbox_xml::RefreshedTrack {
        name: track.title.clone().unwrap_or_else(|| "Unknown".to_string()),
        artist: track.artist.clone().unwrap_or_default(),
        album: track.album.clone().unwrap_or_default(),
        genre,
        kind: rekordbox_xml::ext_to_kind(ext).to_string(),
        size,
        total_time: track.duration.map(|d| d.value()).unwrap_or(0),
        average_bpm,
        tonality,
        track_number: track.track_number,
        disc_number: track.disc_number,
        year,
        label,
        comment,
        date_added,
        bitrate,
        sample_rate,
        location,
        rating: Stars::from_energy(track.energy).to_rating(),
        position_marks,
    }
}

/// The full hash of each available entry of a playlist, in order.
fn entry_hashes(entries: &[PlaylistEntry]) -> Vec<ContentHash> {
    track_hashes(entries)
        .into_iter()
        .map(ContentHash::from)
        .collect()
}

/// Locations to list in a playlist: the `hashes` that resolve to a location
/// which made it into `refreshed_locations`. Hashes of tracks that failed to
/// download are dropped.
///
/// Normalises case at the lookup boundary only; the URI written to
/// rekordbox.xml preserves its original case.
fn playlist_locations(
    hashes: &[ContentHash],
    hash_to_location: &std::collections::HashMap<String, String>,
    refreshed_locations: &std::collections::HashSet<String>,
) -> Vec<String> {
    hashes
        .iter()
        .filter_map(|h| {
            let loc = hash_to_location.get(h.as_str())?;
            if refreshed_locations.contains(&loc.to_lowercase()) {
                Some(loc.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Export tracks to Rekordbox-compatible XML + audio files
///
/// Pass one or more --playlist flags to export named playlists, or `-` to read
/// hashes from stdin (stdin is read only for `-`). Exactly one of the two. The rating of
/// each track is written from its energy (energy 7 is 4 stars). Run
/// `mdma rekordbox import --enrich` before exporting: a rating changed in rekordbox
/// and not yet imported is overwritten by the export.
///
/// # Examples
///
/// ```sh
/// mdma rekordbox export --playlist my-set
/// mdma search --artist CBL | mdma rekordbox export -
/// ```
#[hicli::command]
pub(crate) fn export(args: RekordboxExportArgs, ctx: &Ctx) -> Result<()> {
    use std::collections::HashSet;

    let RekordboxExportArgs {
        output: OutputDir(output),
        format,
        replace: ReplaceXml(replace),
        source,
    } = args;
    // No playlists means the hashes come from stdin (`-`).
    let playlists = match source {
        ExportSource::Playlists(playlists) => playlists,
        ExportSource::Stdin => Vec::new(),
    };
    let library = ctx.node().library()?;
    let output = output.unwrap_or_else(|| {
        dirs::audio_dir()
            .map(|p| p.join("mdma_rekordbox"))
            .unwrap_or_else(|| std::path::PathBuf::from("./rekordbox-export/"))
    });

    let (node, node_ip) = ctx.node().require_node()?;

    // Collect hashes per playlist, or from stdin when no playlists given.
    //
    // hashes_per_playlist: ordered list of (playlist_name, ordered_hashes).
    // When stdin mode, one entry with empty name.
    let hashes_per_playlist: Vec<(String, Vec<ContentHash>)> = if !playlists.is_empty() {
        let mut result = Vec::with_capacity(playlists.len());
        for PlaylistArg(name) in &playlists {
            let pname = name.as_str();
            let entries = library.playlist_entries(name).map_err(library_error)?;
            // An entry that does not resolve is left out of the export, never silently.
            for unavailable in unresolved_entries(&entries) {
                eprintln!("warning: playlist '{}': {}", pname, unavailable);
            }
            result.push((pname.to_string(), entry_hashes(&entries)));
        }
        result
    } else {
        let hashes = require_stdin_hashes_or_usage(&[
            "Usage: mdma search --artist CBL | mdma rekordbox export -",
            "       mdma rekordbox export --playlist my-set",
        ])?;
        vec![("".to_string(), hashes)]
    };

    // Union of all hashes, deduped while preserving first-occurrence order.
    let mut seen: HashSet<ContentHash> = HashSet::new();
    let mut all_hashes: Vec<ContentHash> = Vec::new();
    for (_, hs) in &hashes_per_playlist {
        for h in hs {
            if seen.insert(h.clone()) {
                all_hashes.push(h.clone());
            }
        }
    }

    if all_hashes.is_empty() {
        bail!("All specified playlists are empty.");
    }

    // Resolve each unique hash to full TrackInfo
    let tracks = resolve_tracks_or_abort(&library, all_hashes)?;

    let http = build_http_client(&node, node_ip)?;

    // Create output directory early so we can canonicalise it for stable paths
    if let Err(e) = std::fs::create_dir_all(&output) {
        bail!("Failed to create output directory: {}", e);
    }
    let output_canon = output
        .canonicalize()
        .unwrap_or_else(|_| output.to_path_buf());

    // Resolve format: only --format (no lossless/lossy split for rekordbox export)
    let format_owned = format.unwrap_or(ExportFormat::Original);

    // Load existing XML for incremental sync (skip when --replace). Loaded
    // before planning destinations: its entries keep their paths even when
    // those exceed the Location budget (stable Locations preserve cues).
    let xml_path = output_canon.join("rekordbox.xml");
    let existing: Option<rekordbox_xml::RekordboxLibrary> = if !replace && xml_path.exists() {
        match std::fs::read_to_string(&xml_path) {
            Ok(bytes) => match rekordbox_xml::parse_xml(&bytes) {
                Ok(lib) => Some(lib),
                Err(err) => {
                    bail!(
                        "failed to parse existing rekordbox.xml: {}. Inspect the file or re-run with --replace to regenerate.",
                        err
                    );
                }
            },
            Err(e) => {
                eprintln!("Warning: could not read existing rekordbox.xml ({}); treating as fresh export.", e);
                None
            }
        }
    } else {
        None
    };
    let existing_locations = existing_location_set(existing.as_ref());

    // Build (TrackInfo, canonical_dest_path, ext) for every unique desired track.
    // dest_path is canonicalised so the Location URI is stable pre- and post-download.
    let mut too_long_count = 0usize;
    let desired: Vec<(TrackInfo, std::path::PathBuf, String)> = tracks
        .into_iter()
        .map(|track| {
            let ext = export_extension(&format_owned, &track);
            let (raw_dest, fits) =
                export_dest_path_checked(&output_canon, &track, &ext, &existing_locations);
            too_long_count += usize::from(!fits);
            // Canonicalise if the file already exists; fall back to the absolute path otherwise.
            let dest = raw_dest.canonicalize().unwrap_or_else(|_| raw_dest.clone());
            (track, dest, ext)
        })
        .collect();

    if too_long_count > 0 {
        eprintln!(
            "warning: {} new track path(s) still exceed {} bytes as an encoded rekordbox \
             Location even with artist/album/title shortened to the minimum; the output \
             directory {} ({} bytes) leaves too little room. rekordbox on macOS may reject \
             them. Use a shorter output directory (e.g. ~/mdma-export).",
            too_long_count,
            MAX_LOCATION_URI_BYTES,
            output_canon.display(),
            path_cost(&output_canon.to_string_lossy())
        );
    }

    // hash → canonical dest location URI, for playlist location building.
    // Insert all three hash forms (full, 12-char short, 8-char short) so that
    // playlists stored in any format on disk resolve correctly.
    let hash_location_pairs: Vec<rekordbox_roundtrip::HashLocation> = desired
        .iter()
        .map(|(track, dest, _)| rekordbox_roundtrip::HashLocation {
            hash: track.content_hash.clone(),
            location: rekordbox_xml::path_to_file_uri(dest),
        })
        .collect();
    let hash_to_location = rekordbox_roundtrip::build_hash_to_location(&hash_location_pairs);

    // Build DesiredTrack list for the planner
    let desired_for_plan: Vec<rekordbox_xml::DesiredTrack> = desired
        .iter()
        .map(|(track, dest, _ext)| rekordbox_xml::DesiredTrack {
            dest_path: dest.clone(),
            source_id: track.content_hash.as_str().to_string(),
        })
        .collect();

    let plan = rekordbox_xml::plan_export(existing.as_ref(), &desired_for_plan, |p| p.exists());

    // Warn about planning anomalies
    for collision in &plan.collisions {
        eprintln!(
            "warning: destination path {} is shared by {} tracks ({}); each later track \
             overwrites the previous one, so only the last will remain on disk",
            collision.dest_path.display(),
            collision.source_ids.len(),
            collision.source_ids.join(", ")
        );
    }
    for fc in &plan.format_changes {
        let old_ext = std::path::Path::new(&fc.existing_location)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?");
        let new_ext = fc
            .new_dest_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?");
        eprintln!(
            "warning: {} already exported as .{}; new --format produces .{} — treating as a new track. Use --replace to reset the XML.",
            fc.existing_location, old_ext, new_ext
        );
    }

    // Filter download list to only tracks not already on disk
    let to_download_set: HashSet<std::path::PathBuf> = plan
        .to_download
        .iter()
        .map(|p| p.dest_path.clone())
        .collect();

    let download_tracks_only: Vec<TrackInfo> = desired
        .iter()
        .filter(|(_, dest, _)| to_download_set.contains(dest))
        .map(|(track, _, _)| track.clone())
        .collect();

    let new_count = plan.to_download.len();
    let unchanged_count = plan.to_skip.len();

    // Download to exactly the planned destinations (which may be grandfathered
    // paths longer than the budget), not a re-derived budgeted path.
    let planned_dest: std::collections::HashMap<&str, &std::path::PathBuf> = desired
        .iter()
        .map(|(track, dest, _)| (track.content_hash.as_str(), dest))
        .collect();
    let results = download_tracks(
        &node,
        &download_tracks_only,
        &http,
        |_track| format_owned,
        |track, _ext| planned_dest[track.content_hash.as_str()].clone(),
    );

    let fail_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Failure { .. }))
        .count();

    // Build a map of downloaded track hash → (dest, ext, size) for the refreshed set
    let mut downloaded_map: std::collections::HashMap<String, (std::path::PathBuf, String, u64)> =
        std::collections::HashMap::new();
    for attempt in &results {
        if let DownloadOutcome::Success { dest, ext, size } = &attempt.outcome {
            downloaded_map.insert(
                attempt.track.content_hash.as_str().to_string(),
                (dest.clone(), ext.clone(), *size),
            );
        }
    }

    // Build the skipped-location set for quick membership checks.
    // Keys are lowercased so that case-mismatches between the existing-XML
    // round-trip path and the freshly-built URI path do not miss on-disk tracks.
    let skipped_locations: HashSet<String> =
        plan.to_skip.into_iter().map(|s| s.to_lowercase()).collect();

    // Only include tracks that are either freshly downloaded or already on disk.
    // Failed downloads are logged and dropped — a partial XML is worse than the
    // last-good XML, so we abort before writing if any download failed.
    let mut refreshed: Vec<rekordbox_xml::RefreshedTrack> = Vec::with_capacity(desired.len());
    // What this export writes as the rating of each track: the base of the next import.
    let mut exported: Vec<(ContentHash, Stars)> = Vec::with_capacity(desired.len());
    for (track, dest, ext) in &desired {
        let hash_str = track.content_hash.as_str().to_string();
        let location = rekordbox_xml::path_to_file_uri(dest);

        let resolved = if let Some((dl_dest, dl_ext, dl_size)) = downloaded_map.get(&hash_str) {
            // Freshly downloaded.
            Some((dl_dest.clone(), dl_ext.clone(), *dl_size))
        } else if skipped_locations.contains(&location.to_lowercase()) {
            // Already on disk — get size from filesystem.
            let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            Some((dest.clone(), ext.clone(), size))
        } else {
            // Download failed — skip this track.
            None
        };

        if let Some((actual_dest, actual_ext, size)) = resolved {
            let facts: Vec<(String, String)> = library
                .get_facts(&track.content_hash)
                .map(|(_hash, facts)| facts)
                .unwrap_or_default();
            exported.push((track.content_hash.clone(), Stars::from_energy(track.energy)));
            let audio = ExportedAudio::of_export(&format_owned, &actual_dest, track);
            refreshed.push(build_rekordbox_track(
                track,
                &actual_dest,
                &actual_ext,
                size,
                &facts,
                &audio,
            ));
        }
    }

    // Abort before touching the XML if any download failed; leave the last-good
    // XML in place so the user can re-run after fixing the problem.
    if fail_count > 0 {
        bail!(
            "{} download(s) failed — XML not written. Re-run to retry.",
            fail_count
        );
    }

    if refreshed.is_empty() {
        bail!("No tracks available. Aborting XML generation.");
    }

    // Build a set of locations that actually made it into refreshed.
    // Keys are lowercased so that case-mismatches between the existing-XML
    // round-trip path and the freshly-built URI path do not drop tracks.
    let refreshed_locations: HashSet<String> = refreshed
        .iter()
        .map(|r| r.location.to_lowercase())
        .collect();

    // Build PlaylistUpdate per playlist. Each playlist references its own full track list
    // (not the deduplicated union), so overlapping tracks appear in both playlists.
    // Unresolved hashes (failed downloads) are dropped.
    let playlist_updates: Vec<rekordbox_xml::PlaylistUpdate> = hashes_per_playlist
        .iter()
        .map(|(pname, hs)| rekordbox_xml::PlaylistUpdate {
            name: pname.clone(),
            locations: playlist_locations(hs, &hash_to_location, &refreshed_locations),
        })
        .collect();

    let merged = rekordbox_xml::merge_export(existing, refreshed, &playlist_updates);

    let total_tracks = merged.tracks.len();
    let total_playlists = merged.playlists.len();

    let xml = merged.to_xml();
    std::fs::write(&xml_path, xml.as_bytes())
        .map_err(|e| eyre!("Failed to write rekordbox.xml: {}", e))?;

    // The ratings just written are what was last exchanged (ADR-034).
    rekordbox_exchange::record_export(
        &rekordbox_exchange::state_file()?,
        &rekordbox_exchange::node_key(ctx.node())?,
        &exported,
    )
    .map_err(|e| eyre!("rekordbox.xml is written, but the exchange state is not: {e:#}"))?;

    eprintln!();
    let mode_prefix = if replace { "Replace" } else { "Sync" };
    let total_desired = desired.len();

    if playlists.is_empty() {
        // stdin mode
        eprintln!(
            "{}: {} tracks ({} new, {} already on disk).",
            mode_prefix, total_desired, new_count, unchanged_count
        );
    } else if playlists.len() == 1 {
        eprintln!(
            "{}: {} tracks in playlist '{}' ({} new, {} already on disk).",
            mode_prefix,
            total_desired,
            playlists[0].0.as_str(),
            new_count,
            unchanged_count
        );
    } else {
        // Multi-playlist summary: show per-playlist track counts.
        let per_playlist: Vec<String> = hashes_per_playlist
            .iter()
            .map(|(name, hs)| format!("{}: {}", name, hs.len()))
            .collect();
        eprintln!(
            "{}: {} tracks across {} playlists ({}) ({} new, {} already on disk).",
            mode_prefix,
            total_desired,
            playlists.len(),
            per_playlist.join(", "),
            new_count,
            unchanged_count
        );
    }

    eprintln!(
        "Library now: {} tracks across {} playlists.",
        total_tracks, total_playlists
    );
    eprintln!("XML written to: {}", xml_path.display());

    Ok(())
}

/// What enrich does with a track's beat grid.
#[derive(Debug, PartialEq)]
enum GridUpdate {
    /// The projected grid is already the new one: write nothing.
    Current,
    /// Assert the new grid, first retracting the grid it supersedes (if any) so the
    /// old one cannot resurface when the new one is later retracted. A retract only
    /// cancels a grid from the same source, so grids of other sources stay.
    Write { retract: Option<MusicValue> },
}

fn plan_grid_update(
    new_grid: &MusicValue,
    existing: Option<&library_ipc_client::BeatGridInfo>,
) -> GridUpdate {
    let MusicValue::BeatGrid {
        first_beat_ms,
        bpm,
        beats_per_bar,
    } = new_grid
    else {
        return GridUpdate::Write { retract: None };
    };
    match existing {
        Some(old)
            if old.first_beat_ms == *first_beat_ms
                && old.bpm == *bpm
                && old.beats_per_bar == *beats_per_bar =>
        {
            GridUpdate::Current
        }
        Some(old) => GridUpdate::Write {
            retract: Some(MusicValue::BeatGrid {
                first_beat_ms: old.first_beat_ms,
                bpm: old.bpm,
                beats_per_bar: old.beats_per_bar,
            }),
        },
        None => GridUpdate::Write { retract: None },
    }
}

/// Report the outcome of [`write_playlist`], warning after a replace.
fn print_write_outcome(name: &str, track_count: usize, created: bool) {
    println!("{}", format_write_outcome(name, track_count, created));
}

/// Import tracks and playlists from a Rekordbox XML file
///
/// Matches Rekordbox tracks to MDMA library tracks by metadata,
/// creates playlists, and optionally enriches BPM/Key facts.
///
/// # Examples
///
/// ```sh
/// mdma rekordbox import rekordbox.xml
/// mdma rekordbox import rekordbox.xml --playlist imported-set
/// mdma rekordbox import rekordbox.xml --enrich --dry-run
/// ```
#[hicli::command]
pub(crate) fn import(args: RekordboxImportArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::Bpm;

    let RekordboxImportArgs {
        playlist: ImportPlaylist(playlist),
        all_playlists: AllPlaylists(all_playlists),
        enrich: Enrich(enrich),
        report: ReportFile(report),
        dry_run: DryRun(dry_run),
        path,
    } = args;

    // 1. Read and parse XML file
    let ExistingFileArg(path) = path;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| eyre!("Failed to read {}: {}", path.display(), e))?;

    let parsed = parse_xml(&content).map_err(|e| eyre!("Failed to parse rekordbox XML: {}", e))?;

    if parsed.tracks.is_empty() {
        eprintln!("No tracks found in {}", path.display());
        return Ok(());
    }

    // 2. Create lookup and match tracks. The matches become writes to the node, so
    // they are asked of the node whatever --facts says (ADR-030 §5): a stale replica
    // would give fewer matches and the import would replace a good playlist with a
    // partial one.
    let facts = Facts::Node(ctx.node());
    let lookup = LibraryTrackLookup::new(|q| facts.search(q));

    // The one matching pass: everything below (summary, playlists, enrichment, report)
    // is derived from these results.
    let mut results: Vec<(&RekordboxTrack, MatchResult)> = Vec::new();

    for track in &parsed.tracks {
        let candidate = CandidateTrack {
            isrc: None,
            artist: if track.artist.is_empty() {
                None
            } else {
                Some(track.artist.clone())
            },
            title: if track.name.is_empty() {
                None
            } else {
                Some(track.name.clone())
            },
            duration_secs: if track.total_time > 0 {
                Some(track.total_time)
            } else {
                None
            },
        };

        results.push((track, track_matcher::match_track(&candidate, &lookup)));
    }

    // A failed library search must not look like "not in library"
    lookup.check()?;

    // The writes (playlists, enrichment) go through the node too.
    let library = ctx.node().library()?;

    // The exchange state is read before anything is written, so that a damaged file
    // stops the import before it has changed anything (ADR-034).
    let mut energy = if enrich {
        Some(rekordbox_exchange::EnergySync::load(
            &rekordbox_exchange::state_file()?,
            &rekordbox_exchange::node_key(ctx.node())?,
            dry_run,
        )?)
    } else {
        None
    };

    // 3. Print summary
    let summary = rekordbox_report::summarize(&results);
    debug_assert_eq!(summary.total(), parsed.tracks.len());
    // Definitive and weak matches both go into playlists and get enriched.
    let matched = rekordbox_report::matched(&results);
    println!("Matched:   {:3} tracks", matched.len());
    println!(
        "Ambiguous: {:3} tracks (need manual resolution)",
        summary.ambiguous
    );
    println!("Unmatched: {:3} tracks", summary.unmatched);

    // 4. The report rows, from the same match results. The candidates' details are
    // only fetched for a report: they are not needed otherwise.
    let problems = rekordbox_report::problems(&results, |hash| {
        if report.is_none() {
            return Candidate::bare(hash.clone());
        }
        match library.get_track(hash) {
            Ok(info) => Candidate::from_track(&info),
            Err(e) => Candidate::failed(hash.clone(), e.to_string()),
        }
    });
    let lookup_failures = problems
        .iter()
        .flat_map(|p| p.candidates())
        .filter(|c| c.lookup_failed())
        .count();
    // What `--all-playlists` would do with the playlists: written below, and the
    // source of the name problems in the report.
    let mut plan = rekordbox_report::plan_playlists(
        &parsed.playlists,
        &results,
        playlist.as_ref().map(|p| p.name()),
    );
    let covers_playlists = all_playlists || report.is_some();
    let report_data = rekordbox_report::Report {
        problems,
        // What each playlist loses; the report covers every playlist of the XML.
        playlists: if covers_playlists {
            rekordbox_report::tally_playlists(&parsed.playlists, &results)
        } else {
            vec![]
        },
        // The playlists an `--all-playlists` import would write.
        issues: if covers_playlists {
            std::mem::take(&mut plan.issues)
        } else {
            rekordbox_report::NameIssues::default()
        },
    };

    if let Some(report_path) = &report {
        // Written with --dry-run too: it is a report, not a change to the library.
        std::fs::write(report_path, rekordbox_report::render_tsv(&report_data))
            .map_err(|e| eyre!("Failed to write report {}: {}", report_path.display(), e))?;
        println!(
            "{} unmatched, {} ambiguous, {} weak, {} shared \u{2192} report written to {}",
            summary.unmatched,
            summary.ambiguous,
            summary.weak,
            summary.shared,
            report_path.display()
        );
        if lookup_failures > 0 {
            eprintln!(
                "{lookup_failures} candidate lookup(s) failed; marked `<lookup failed: ..>` in the report"
            );
        }
    } else {
        // 4a. Print unmatched tracks to stderr
        let unmatched: Vec<&RekordboxTrack> = report_data
            .problems
            .iter()
            .filter_map(|p| match p {
                Problem::Unmatched { track, .. } => Some(*track),
                _ => None,
            })
            .collect();
        if !unmatched.is_empty() {
            eprintln!();
            eprintln!("Unmatched tracks:");
            for t in &unmatched {
                let location = rekordbox_report::location_path(t);
                let filename = std::path::Path::new(&location)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&location);
                eprintln!("  {} - {} ({})", t.artist, t.name, filename);
            }
        }

        // 4b. Print ambiguous tracks to stderr
        let ambiguous: Vec<(&RekordboxTrack, &[Candidate])> = report_data
            .problems
            .iter()
            .filter_map(|p| match p {
                Problem::Ambiguous {
                    track, candidates, ..
                } => Some((*track, candidates.as_slice())),
                _ => None,
            })
            .collect();
        if !ambiguous.is_empty() {
            eprintln!();
            eprintln!("Ambiguous tracks (multiple candidates):");
            for (t, candidates) in &ambiguous {
                eprintln!("  {} - {}", t.artist, t.name);
                for c in candidates.iter().take(3) {
                    let h = c.hash.as_str();
                    eprintln!("    candidate: {}", h.get(..16).unwrap_or(h));
                }
            }
        }
        if summary.weak + summary.shared > 0 {
            eprintln!();
            eprintln!(
                "{} weak (duration differs) and {} shared (several tracks, one library track) \
                 matches are not listed; use --report to see them",
                summary.weak, summary.shared
            );
        }
    }

    let losing: Vec<_> = report_data
        .playlists
        .iter()
        .filter(|d| d.dropped() > 0)
        .collect();
    if !losing.is_empty() {
        eprintln!();
        eprintln!(
            "Playlist entries dropped (track unmatched, ambiguous or not in the collection):"
        );
        for d in losing {
            eprintln!("  {}: {}", d.playlist, d.describe());
        }
    }
    for c in report_data
        .issues
        .collisions
        .iter()
        .filter(|_| all_playlists)
    {
        eprintln!(
            "Playlists {} all become '{}'; each replaces the one before",
            c.playlists
                .iter()
                .map(|p| format!("'{p}'"))
                .collect::<Vec<_>>()
                .join(", "),
            c.name
        );
    }

    // 6. Create --playlist if requested
    if let Some(arg) = playlist {
        if let Some(note) = arg.change_note() {
            eprintln!("{note}");
        }
        let name = arg.name();
        let sanitized = name.as_str();
        let hashes: Vec<ContentHash> = matched.iter().map(|(_, h)| h.clone()).collect();

        if dry_run {
            println!(
                "[dry-run] Would create playlist '{}' with {} tracks",
                sanitized,
                hashes.len()
            );
        } else {
            let created = write_playlist(&library, name, &hashes)
                .map_err(|e| eyre!("Failed to write playlist: {}", e))?;
            print_write_outcome(sanitized, hashes.len(), created);
        }
    }

    // 7. Create --all-playlists
    if all_playlists {
        for rb_playlist in &plan.nothing_matched {
            eprintln!(
                "Skipping playlist '{}': no matched tracks",
                rb_playlist.name
            );
        }
        for skipped in &report_data.issues.skipped {
            eprintln!(
                "Skipping playlist '{}': {}",
                skipped.playlist, skipped.reason
            );
        }
        for planned in &plan.writes {
            let rb_playlist = planned.source;
            let target = &planned.target;
            let resolved = &planned.hashes;
            let sanitized = target.name().as_str();

            if dry_run {
                println!(
                    "[dry-run] Would create playlist '{}' (from '{}') with {} tracks",
                    sanitized,
                    rb_playlist.name,
                    resolved.len()
                );
            } else {
                match write_playlist(&library, target.name(), resolved) {
                    Ok(created) => print_write_outcome(sanitized, resolved.len(), created),
                    Err(e) => {
                        eprintln!("Failed to write playlist '{}': {}", sanitized, e)
                    }
                }
            }
        }
    }

    // 8. Enrich BPM/Key facts
    if enrich {
        let mut enriched = 0usize;
        let mut skipped = 0usize;
        let mut errors = 0usize;

        let source = library_ipc_client::rekordbox_source(env!("CARGO_PKG_VERSION"));

        for (rb_track, hash) in &matched {
            // Fetch current track facts; on failure fall back to writing (safe default).
            let fetched = library.get_track(hash);
            let existing = fetched.as_ref().ok();

            // Write BPM if available
            if let Some(bpm_f32) = rb_track.average_bpm {
                if let Ok(bpm) = Bpm::from_f32(bpm_f32) {
                    let already_current = existing
                        .as_ref()
                        .and_then(|t| t.bpm)
                        .map(|existing_bpm| existing_bpm == bpm)
                        .unwrap_or(false);

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write BPM {} for {} - {}",
                            bpm_f32, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact_as(hash, MusicValue::Bpm(bpm), &source) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write BPM for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            // Write Key (Tonality) if available
            if let Some(tonality) = rb_track
                .tonality
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                let key = parse_tonality(tonality);
                if key.is_none() {
                    eprintln!(
                        "warning: unrecognised Tonality '{}' for {} - {}; no Key written",
                        tonality, rb_track.artist, rb_track.name
                    );
                    errors += 1;
                }
                if let Some(key) = key {
                    let already_current = existing
                        .as_ref()
                        .and_then(|t| t.key)
                        .map(|existing_key| existing_key == key)
                        .unwrap_or(false);

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write Key {} for {} - {}",
                            tonality, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact_as(hash, MusicValue::Key(key), &source) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write Key for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            // Write BeatGrid from tempo anchor (Rekordbox is beat-grid master;
            // MDMA never writes <TEMPO> data back on export).
            let average_bpm = rb_track.average_bpm.and_then(|b| Bpm::from_f32(b).ok());
            let projected_grid = rb_track
                .tempo_anchor
                .as_ref()
                .and_then(|anchor| rekordbox_roundtrip::grid_from_tempo(anchor, average_bpm));
            if let Some(new_grid) = projected_grid {
                // Guard: skip if the projected beat_grid is already identical.
                let plan = plan_grid_update(
                    &new_grid,
                    existing.as_ref().and_then(|t| t.beat_grid.as_ref()),
                );
                match plan {
                    GridUpdate::Current => skipped += 1,
                    GridUpdate::Write { .. } if dry_run => {
                        println!(
                            "[dry-run] Would write {:?} for {} - {}",
                            new_grid, rb_track.artist, rb_track.name
                        );
                    }
                    GridUpdate::Write { retract } => {
                        // Retract the superseded grid first, so it cannot
                        // resurface when the new one is retracted later.
                        let retracted = match retract {
                            Some(old) => library.retract_fact_as(hash, old, &source).map(|_| ()),
                            None => Ok(()),
                        };
                        match retracted
                            .and_then(|()| library.write_fact_as(hash, new_grid, &source))
                        {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write BeatGrid for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            // Write MemoryCue facts from position marks.
            // Guard: skip any cue that already exists identically in the projection.
            if !rb_track.position_marks.is_empty() {
                let cues = rekordbox_roundtrip::cues_from_position_marks(&rb_track.position_marks);
                for cue in cues {
                    let already_current = if let MusicValue::MemoryCue {
                        position_ms,
                        kind,
                        label,
                        index,
                    } = &cue
                    {
                        existing
                            .as_ref()
                            .map(|t| {
                                t.memory_cues.iter().any(|c| {
                                    c.position_ms == *position_ms
                                        && &c.kind == kind
                                        && c.label == *label
                                        && c.index == *index
                                })
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    };

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write {} for {} - {}",
                            cue, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact_as(hash, cue, &source) {
                            Ok(()) => enriched += 1,
                            Err(e) => {
                                eprintln!(
                                    "Failed to write MemoryCue for {} - {}: {}",
                                    rb_track.artist, rb_track.name, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
            }

            if let Some(energy) = energy.as_mut() {
                energy.track(&library, rb_track, hash, fetched.as_ref());
            }
        }

        if !dry_run {
            println!(
                "Enriched {} fact(s), skipped {} (already up to date), {} error(s)",
                enriched, skipped, errors
            );
        }

        // Rating ↔ energy (ADR-034): stdout gets the summary, stderr the conflicts.
        if let Some(energy) = energy {
            let report = energy.finish()?;
            for line in &report.would {
                println!("[dry-run] {line}");
            }
            println!("{}", report.summary(dry_run));
            if !report.conflicts.is_empty() {
                eprintln!();
                eprintln!("Energy conflicts (kept MDMA):");
                for conflict in &report.conflicts {
                    eprintln!("{}", conflict.line());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::playlist_locations;
    use library_ipc_client::ContentHash;
    use music_facts::MusicValue;
    use pretty_assertions::assert_eq;

    // ── argv parsing ─────────────────────────────────────────────────────────────

    use crate::test_support::{parses, run_arg, usage_error};

    #[test]
    fn export_args_parser_reads_playlists_output_format_and_replace() {
        let args = run_arg::<super::RekordboxExportArgs>(&[
            "--playlist",
            "set-1",
            "--playlist",
            "set_2",
            "--output",
            "/tmp/out",
            "--format",
            "wav",
            "--replace",
        ])
        .unwrap();
        let super::ExportSource::Playlists(playlists) = &args.source else {
            panic!("playlists expected: {:?}", args.source);
        };
        let names: Vec<&str> = playlists.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(names, ["set-1", "set_2"]);
        assert_eq!(args.output.0, Some(std::path::PathBuf::from("/tmp/out")));
        assert_eq!(args.format, Some(super::ExportFormat::Wav));
        assert!(args.replace.0);

        let none = run_arg::<super::RekordboxExportArgs>(&["-"]).unwrap();
        assert_eq!(none.source, super::ExportSource::Stdin);
        assert_eq!(none.output.0, None);
        assert_eq!(none.format, None);
        assert!(!none.replace.0);
    }

    #[test]
    fn export_args_parser_validates_names_and_formats() {
        let err = run_arg::<super::RekordboxExportArgs>(&["--playlist", "has space"]).unwrap_err();
        assert!(err.contains("invalid characters"), "{err}");
        assert!(err.contains("`--playlist`"), "{err}");
        let err = run_arg::<super::RekordboxExportArgs>(&["--format", "flac", "-"]).unwrap_err();
        assert!(err.contains("`--format`"), "{err}");
        assert!(
            err.contains("expected `original`, `aiff` or `wav`"),
            "{err}"
        );
    }

    #[test]
    fn import_args_parser_reads_the_file_last() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let args = run_arg::<super::RekordboxImportArgs>(&[
            "--playlist",
            "My Set",
            "--all-playlists",
            "--enrich",
            "--dry-run",
            path,
        ])
        .unwrap();
        assert_eq!(args.playlist.0.unwrap().name().as_str(), "My-Set");
        assert!(args.all_playlists.0 && args.enrich.0 && args.dry_run.0);
        assert_eq!(args.path.0, file.path());

        assert_eq!(args.report.0, None);

        let reported = run_arg::<super::RekordboxImportArgs>(&[
            "--report",
            "/tmp/missing.tsv",
            "--dry-run",
            path,
        ])
        .unwrap();
        assert_eq!(
            reported.report.0,
            Some(std::path::PathBuf::from("/tmp/missing.tsv"))
        );
        assert!(reported.dry_run.0);

        let plain = run_arg::<super::RekordboxImportArgs>(&[path]).unwrap();
        assert_eq!(plain.report.0, None);
        assert_eq!(plain.playlist.0, None);
        assert!(!plain.all_playlists.0 && !plain.enrich.0 && !plain.dry_run.0);
    }

    #[test]
    fn import_args_parser_checks_the_file_and_the_name() {
        let err = run_arg::<super::RekordboxImportArgs>(&["/definitely/not/here.xml"]).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let err = run_arg::<super::RekordboxImportArgs>(&["--playlist", "///", path]).unwrap_err();
        assert!(err.contains("name sanitizes to empty"), "{err}");
        assert!(run_arg::<super::RekordboxImportArgs>(&[]).is_err());
    }

    #[test]
    fn export_playlist_names_are_validated_at_parse_time() {
        for bad in ["has space", "a/b", ""] {
            let err = usage_error(&["rekordbox", "export", "--playlist", bad]);
            assert!(err.contains("couldn't parse"), "{bad:?}: {err}");
        }
        // One bad name among good ones is still rejected.
        usage_error(&[
            "rekordbox",
            "export",
            "--playlist",
            "good",
            "--playlist",
            "not good",
        ]);
    }

    #[test]
    fn export_accepts_valid_playlist_names() {
        assert_eq!(
            parses(&[
                "rekordbox",
                "export",
                "--playlist",
                "set-1",
                "--playlist",
                "set_2",
            ]),
            ["rekordbox", "export"]
        );
    }

    #[test]
    fn export_reads_stdin_only_for_a_dash() {
        assert_eq!(
            parses(&["rekordbox", "export", "-"]),
            ["rekordbox", "export"]
        );
        assert_eq!(
            parses(&["rekordbox", "export", "--format", "wav", "-"]),
            ["rekordbox", "export"]
        );
        usage_error(&["rekordbox", "export", "-", "-"]);
        let dash = run_arg::<super::RekordboxExportArgs>(&["-"]).unwrap();
        assert_eq!(dash.source, super::ExportSource::Stdin);
    }

    #[test]
    fn export_needs_exactly_one_source_of_tracks() {
        // One: playlists, or the dash.
        assert_eq!(
            parses(&["rekordbox", "export", "--playlist", "set"]),
            ["rekordbox", "export"]
        );
        assert_eq!(
            parses(&["rekordbox", "export", "--playlist", "a", "--playlist", "b"]),
            ["rekordbox", "export"]
        );
        // Neither: a usage error naming both ways.
        let neither = usage_error(&["rekordbox", "export"]);
        assert!(neither.contains("--playlist"), "{neither}");
        let neither = usage_error(&["rekordbox", "export", "--format", "wav"]);
        assert!(neither.contains("--playlist"), "{neither}");
        // Both: a usage error too, in either order.
        usage_error(&["rekordbox", "export", "--playlist", "set", "-"]);
        usage_error(&["rekordbox", "export", "-", "--playlist", "set"]);
    }

    #[test]
    fn import_rejects_a_missing_file() {
        let err = usage_error(&["rekordbox", "import", "/definitely/not/here.xml"]);
        assert!(err.contains("does not exist"), "message: {err}");
    }

    #[test]
    fn import_rejects_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = usage_error(&["rekordbox", "import", dir.path().to_str().unwrap()]);
        assert!(err.contains("`PATH`: not a file"), "message: {err}");
    }

    #[test]
    fn import_playlist_that_sanitizes_to_nothing_is_rejected() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        for bad in ["", "///", " "] {
            let err = usage_error(&["rekordbox", "import", path, "--playlist", bad]);
            assert!(err.contains("name sanitizes to empty"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn import_accepts_existing_file_and_sanitizes_playlist() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let argv = ["rekordbox", "import", path, "--playlist", "My Set!"];
        assert_eq!(parses(&argv), ["rekordbox", "import"]);
        let args = run_arg::<super::RekordboxImportArgs>(&["--playlist", "My Set!", path]).unwrap();
        assert_eq!(args.path.0, file.path());
        let playlist = args.playlist.0.unwrap();
        assert_eq!(
            playlist.change_note().as_deref(),
            Some("Note: playlist name sanitized from 'My Set!' to 'My-Set'")
        );
        assert_eq!(playlist.name().as_str(), "My-Set");
    }

    // ── rating ───────────────────────────────────────────────────────────────────

    /// ADR-034: the export writes the stars of the energy as the rating (7 is 4 stars,
    /// 204), and no energy is unrated.
    #[test]
    fn export_writes_the_energy_as_a_rating() {
        let hash = format!("sha256:{}", "a".repeat(64));
        for (energy, rating) in [(None, 0), (Some(1), 51), (Some(7), 204), (Some(10), 255)] {
            let track = library_ipc_client::TrackInfo {
                energy: energy.map(|n| library_ipc_client::EnergyLevel::new(n).unwrap()),
                ..crate::test_support::track(&hash)
            };
            let built = super::build_rekordbox_track(
                &track,
                std::path::Path::new("/x/a.aiff"),
                "aiff",
                1,
                &[],
                &super::ExportedAudio::of_source(&track),
            );
            assert_eq!(built.rating, rating, "energy {energy:?}");
        }
    }

    // ── beat grid re-import ──────────────────────────────────────────────────────

    fn grid_value(first_beat_ms: u32) -> MusicValue {
        MusicValue::BeatGrid {
            first_beat_ms,
            bpm: library_ipc_client::Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        }
    }

    fn grid_info(first_beat_ms: u32) -> library_ipc_client::BeatGridInfo {
        library_ipc_client::BeatGridInfo {
            first_beat_ms,
            bpm: library_ipc_client::Bpm::from_u32(120).unwrap(),
            beats_per_bar: 4,
        }
    }

    #[test]
    fn a_first_grid_is_written_without_a_retract() {
        assert_eq!(
            super::plan_grid_update(&grid_value(1_000), None),
            super::GridUpdate::Write { retract: None }
        );
    }

    #[test]
    fn an_identical_grid_is_current() {
        assert_eq!(
            super::plan_grid_update(&grid_value(1_000), Some(&grid_info(1_000))),
            super::GridUpdate::Current
        );
    }

    #[test]
    fn a_corrected_grid_retracts_the_superseded_one() {
        assert_eq!(
            super::plan_grid_update(&grid_value(2_500), Some(&grid_info(1_000))),
            super::GridUpdate::Write {
                retract: Some(grid_value(1_000))
            }
        );
    }

    /// #122: the bit rate and sample rate come from the projected track, not from
    /// the display strings of `get_facts` ("1411 kbps", "44100 Hz"), which do not
    /// parse as numbers and used to be written as 0.
    #[test]
    fn export_writes_the_bitrate_and_sample_rate_of_the_track() {
        use library_ipc_client::{AudioProperties, Bitrate, SampleRate};
        let hash = format!("sha256:{}", "a".repeat(64));
        let track = library_ipc_client::TrackInfo {
            audio: AudioProperties {
                bitrate: Some(Bitrate::new(1411)),
                sample_rate: Some(SampleRate::new(44100)),
                ..AudioProperties::default()
            },
            ..crate::test_support::track(&hash)
        };
        // The facts as `get_facts` renders them: display strings, not numbers.
        let facts = [
            ("Bitrate".to_string(), "1411 kbps".to_string()),
            ("SampleRate".to_string(), "44100 Hz".to_string()),
        ];
        let built = super::build_rekordbox_track(
            &track,
            std::path::Path::new("/x/a.aiff"),
            "aiff",
            1,
            &facts,
            &super::ExportedAudio::of_source(&track),
        );
        let xml = rekordbox_xml::merge_export(None, vec![built], &[]).to_xml();
        assert!(xml.contains("BitRate=\"1411\""), "{xml}");
        assert!(xml.contains("SampleRate=\"44100\""), "{xml}");
    }

    /// A 96 kHz FLAC exported as 16-bit/48 kHz AIFF states the exported file's
    /// rates, not the source's.
    #[test]
    fn export_writes_the_format_of_the_exported_file_not_the_source() {
        use library_ipc_client::{AudioProperties, Bitrate, SampleRate};
        let hash = format!("sha256:{}", "a".repeat(64));
        let track = library_ipc_client::TrackInfo {
            audio: AudioProperties {
                bitrate: Some(Bitrate::new(3000)),
                sample_rate: Some(SampleRate::new(96000)),
                ..AudioProperties::default()
            },
            ..crate::test_support::track(&hash)
        };
        let built = super::build_rekordbox_track(
            &track,
            std::path::Path::new("/x/a.aiff"),
            "aiff",
            1,
            &[],
            &super::ExportedAudio::of_pcm(48000, 16, 2),
        );
        let xml = rekordbox_xml::merge_export(None, vec![built], &[]).to_xml();
        assert!(xml.contains("BitRate=\"1536\""), "{xml}");
        assert!(xml.contains("SampleRate=\"48000\""), "{xml}");
    }

    #[test]
    fn pcm_audio_is_computed_from_rate_depth_and_channels() {
        for (rate, depth, channels, kbps) in [
            (44_100, 16, 2, 1411),
            (44_100, 24, 2, 2116),
            (48_000, 16, 2, 1536),
            (48_000, 24, 2, 2304),
        ] {
            assert_eq!(
                super::ExportedAudio::of_pcm(rate, depth, channels),
                super::ExportedAudio {
                    bitrate: Some(kbps),
                    sample_rate: Some(rate)
                }
            );
        }
    }

    #[test]
    fn an_original_export_keeps_the_source_values() {
        use library_ipc_client::{AudioProperties, Bitrate, SampleRate};
        let hash = format!("sha256:{}", "a".repeat(64));
        let track = library_ipc_client::TrackInfo {
            audio: AudioProperties {
                bitrate: Some(Bitrate::new(320)),
                sample_rate: Some(SampleRate::new(44100)),
                ..AudioProperties::default()
            },
            ..crate::test_support::track(&hash)
        };
        // The path does not exist: an Original export must not read it.
        let audio = super::ExportedAudio::of_export(
            &crate::transfer::ExportFormat::Original,
            std::path::Path::new("/nonexistent/a.aiff"),
            &track,
        );
        assert_eq!(audio, super::ExportedAudio::of_source(&track));
        assert_eq!(audio.bitrate, Some(320));
    }

    /// A PCM export whose format cannot be read back states no rates rather than
    /// the source's, which describe a different file.
    #[test]
    fn an_unreadable_pcm_export_omits_the_rates() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let track = crate::test_support::track(&hash);
        let audio = super::ExportedAudio::of_export(
            &crate::transfer::ExportFormat::Aiff,
            std::path::Path::new("/nonexistent/a.aiff"),
            &track,
        );
        assert_eq!(
            audio,
            super::ExportedAudio {
                bitrate: None,
                sample_rate: None
            }
        );
    }

    #[test]
    fn tonality_in_any_notation_is_a_key() {
        use library_ipc_client::Key;
        for (tonality, expected) in [
            ("Abm", "G#m"),
            ("C", "C"),
            ("F#m", "F#m"),
            ("Bb", "A#"),
            ("8A", "Am"),
            ("11B", "A"),
        ] {
            let key = super::parse_tonality(tonality)
                .unwrap_or_else(|| panic!("{tonality} should parse"));
            assert_eq!(
                key,
                expected.parse::<Key>().unwrap(),
                "{tonality} vs {expected}"
            );
        }
    }

    #[test]
    fn unparseable_tonality_is_not_a_key() {
        assert_eq!(super::parse_tonality("H#"), None);
        assert_eq!(super::parse_tonality("nonsense"), None);
    }

    #[test]
    fn export_help_says_to_import_first() {
        let help = crate::test_support::help(&["rekordbox", "export", "--help"]);
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(help.contains("rekordbox import --enrich"), "{help}");
        assert!(help.contains("before exporting"), "{help}");
    }

    // ── playlist entries ─────────────────────────────────────────────────────────

    /// A playlist line stored short resolves, through the library, to the track's full
    /// hash; the locations are looked up by that, and an unresolved entry has none.
    #[test]
    fn rekordbox_playlist_locations_use_canonical_hashes() {
        use library_ipc_client::{CanonicalHash, EntryError, PlaylistEntry};
        let full = ContentHash::new(format!("sha256:{}", "a".repeat(64)));
        let location = "file://localhost/tmp/export/a.aiff".to_string();
        let hash_to_location =
            rekordbox_roundtrip::build_hash_to_location(&[rekordbox_roundtrip::HashLocation {
                hash: full.clone(),
                location: location.clone(),
            }]);
        let refreshed: std::collections::HashSet<String> = [location.clone()].into_iter().collect();
        let entries = [
            PlaylistEntry::Unresolved {
                error: EntryError::Unknown {
                    hash: ContentHash::new("bbbbbbbb"),
                },
            },
            PlaylistEntry::Track {
                hash: CanonicalHash::try_from(full).unwrap(),
            },
        ];
        assert_eq!(
            playlist_locations(
                &super::entry_hashes(&entries),
                &hash_to_location,
                &refreshed
            ),
            [location]
        );
    }

    // ── case-insensitive location URI matching ───────────────────────────────────

    /// Bug 1 regression: when the existing XML round-trip produces a location URI
    /// with different case than the freshly-built URI, the playlist filter must
    /// still match it.  Before the fix, `refreshed_locations` was a case-sensitive
    /// `HashSet<&str>`, so the case-mismatched probe returned false and the track
    /// was silently dropped from the playlist.
    ///
    /// This test exercises `playlist_locations`: it builds a `refreshed_locations`
    /// set whose location uses lowercase, and a `hash_to_location` map whose
    /// location uses mixed-case (as produced by the existing-XML round-trip), then
    /// asserts that the playlist's locations vector is non-empty.
    #[test]
    fn rekordbox_export_associates_tracks_when_locations_differ_in_case() {
        // The existing XML round-trip canonicalises to a mixed-case URI.
        let hash = "sha256:abcdef1234567890aabbccddeeff00112233445566778899aabbccddeeff0011";
        let location_upper = "file://Localhost/tmp/export/Polyrytmi.aiff".to_string();
        // The freshly-emitted RefreshedTrack uses the canonical lowercase form.
        let location_lower = "file://localhost/tmp/export/polyrytmi.aiff".to_string();

        // refreshed_locations built from freshly-emitted tracks (lowercase).
        let refreshed_locations: std::collections::HashSet<String> = [location_lower.clone()]
            .into_iter()
            .map(|s| s.to_lowercase())
            .collect();

        // hash_to_location built from existing-XML round-trip (mixed-case).
        let hash_to_location: std::collections::HashMap<String, String> =
            [(hash.to_string(), location_upper.clone())]
                .into_iter()
                .collect();

        let hashes = [ContentHash::new(hash)];
        let locations = playlist_locations(&hashes, &hash_to_location, &refreshed_locations);

        assert_eq!(
            locations,
            std::slice::from_ref(&location_upper),
            "playlist must reference the track even when URI case differs between \
             existing-XML round-trip and freshly-emitted path"
        );
    }

    /// Bug 2 regression: when the existing XML holds a location URI with different
    /// case than the freshly-built URI, `merge_export` must treat them as the same
    /// track — not create a duplicate COLLECTION entry.
    #[test]
    fn rekordbox_export_does_not_duplicate_when_existing_xml_location_differs_in_case() {
        // Round 1: existing XML holds a mixed-case location.
        let loc_mixed = "file://Localhost/tmp/export/Polyrytmi.aiff".to_string();
        // Round 2: freshly-emitted RefreshedTrack uses the canonical lowercase form.
        let loc_lower = "file://localhost/tmp/export/polyrytmi.aiff".to_string();

        // Build a minimal existing library with the mixed-case location.
        let existing = rekordbox_xml::RekordboxLibrary {
            tracks: vec![rekordbox_xml::RekordboxTrack {
                track_id: 1,
                name: "Polyrytmi".to_string(),
                artist: "Carbon Based Lifeforms".to_string(),
                album: String::new(),
                genre: String::new(),
                kind: "AIFF File".to_string(),
                size: 1000,
                total_time: 360,
                average_bpm: None,
                tonality: None,
                track_number: None,
                disc_number: None,
                year: None,
                label: None,
                comment: None,
                date_added: None,
                bitrate: None,
                sample_rate: None,
                location: loc_mixed.clone(),
                tempo_anchor: None,
                rating: 0,
                position_marks: vec![],
            }],
            playlists: vec![],
        };

        // Freshly-refreshed track uses the lowercase canonical URI.
        let refreshed = rekordbox_xml::RefreshedTrack {
            name: "Polyrytmi".to_string(),
            artist: "Carbon Based Lifeforms".to_string(),
            album: String::new(),
            genre: String::new(),
            kind: "AIFF File".to_string(),
            size: 1000,
            total_time: 360,
            average_bpm: None,
            tonality: None,
            track_number: None,
            disc_number: None,
            year: None,
            label: None,
            comment: None,
            date_added: None,
            bitrate: None,
            sample_rate: None,
            location: loc_lower.clone(),
            rating: 0,
            position_marks: vec![],
        };

        // Without case normalisation in merge_export, the existing mixed-case
        // location and the new lowercase location are treated as different tracks,
        // yielding 2 COLLECTION entries instead of 1.
        // This test asserts the merged library has exactly ONE entry.
        let merged = rekordbox_xml::merge_export(Some(existing), vec![refreshed], &[]);

        assert_eq!(
            merged.tracks.len(),
            1,
            "merge_export must not create a duplicate when existing and refreshed \
             locations differ only in case; got tracks: {:?}",
            merged
                .tracks
                .iter()
                .map(|t| &t.location)
                .collect::<Vec<_>>()
        );
    }
}
