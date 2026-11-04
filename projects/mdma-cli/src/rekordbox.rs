use crate::args::{ExistingFileArg, PlaylistArg, SanitizedPlaylistName};
use crate::ctx::Ctx;
use crate::stdin::{read_hashes, resolve_tracks_skip_errors};
use crate::transfer::{
    build_http_client, download_tracks, existing_location_set, export_dest_path_checked,
    export_extension, path_cost, DownloadOutcome, ExportFormat, MAX_LOCATION_URI_BYTES,
};
use clap::Subcommand;
use color_eyre::Result;
use library_ipc_client::{ContentHash, TrackInfo};
use music_facts::MusicValue;
use playlist_import::{format_write_outcome, write_playlist, LibraryTrackLookup};
use rekordbox_xml::{parse_xml, RekordboxTrack};
use track_matcher::{CandidateTrack, MatchResult};

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Export tracks to Rekordbox-compatible XML + audio files.
    ///
    /// Pass one or more --playlist flags to export named playlists.
    /// When no --playlist is given, hashes are read from stdin instead.
    /// Stdin input is ignored when --playlist is provided.
    Export {
        /// MDMA playlist(s) to export. Repeat for multiple: --playlist A --playlist B.
        /// If none given, falls back to stdin input (existing behaviour).
        #[arg(long)]
        playlist: Vec<PlaylistArg>,
        /// Output directory. Defaults to ~/Music/mdma_rekordbox.
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Audio format (defaults to original)
        #[arg(long, value_enum)]
        format: Option<ExportFormat>,
        /// Force a fresh XML and re-download everything (discards incremental sync).
        /// Also skips grandfathering of existing paths: tracks whose paths exceed
        /// the rekordbox Location budget get new, shortened paths (rekordbox will
        /// see them as new tracks and cues are lost).
        #[arg(long)]
        replace: bool,
    },

    /// Import tracks and playlists from a Rekordbox XML file.
    ///
    /// Matches Rekordbox tracks to MDMA library tracks by metadata,
    /// creates playlists, and optionally enriches BPM/Key facts.
    ///
    /// Examples:
    ///   mdma rekordbox import rekordbox.xml
    ///   mdma rekordbox import rekordbox.xml --playlist imported-set
    ///   mdma rekordbox import rekordbox.xml --enrich --dry-run
    Import {
        /// Path to rekordbox.xml file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        path: ExistingFileArg,

        /// Create an MDMA playlist with matched tracks.
        /// Invalid characters (spaces, special chars) are replaced with `-`.
        #[arg(long)]
        playlist: Option<SanitizedPlaylistName>,

        /// Import all playlists found in the XML as MDMA playlists
        #[arg(long)]
        all_playlists: bool,

        /// Update BPM and Key facts from Rekordbox data for matched tracks
        #[arg(long)]
        enrich: bool,

        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },
}

pub(crate) fn dispatch(command: Command, ctx: &Ctx) -> Result<()> {
    match command {
        Command::Export {
            playlist,
            output,
            format,
            replace,
        } => export(
            RekordboxExportArgs {
                playlist,
                output,
                format,
                replace,
            },
            ctx,
        ),
        Command::Import {
            path,
            playlist,
            all_playlists,
            enrich,
            dry_run,
        } => import(
            RekordboxImportArgs {
                playlist,
                all_playlists,
                enrich,
                dry_run,
                path,
            },
            ctx,
        ),
    }
}

/// Values of `rekordbox export`; playlist names are validated at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RekordboxExportArgs {
    pub(crate) playlist: Vec<PlaylistArg>,
    pub(crate) output: Option<std::path::PathBuf>,
    pub(crate) format: Option<ExportFormat>,
    pub(crate) replace: bool,
}

/// Values of `rekordbox import`; the file must exist and the playlist name is
/// sanitised, both at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RekordboxImportArgs {
    pub(crate) playlist: Option<SanitizedPlaylistName>,
    pub(crate) all_playlists: bool,
    pub(crate) enrich: bool,
    pub(crate) dry_run: bool,
    pub(crate) path: ExistingFileArg,
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
) -> rekordbox_xml::RefreshedTrack {
    let find_fact = |name: &str| -> Option<String> {
        facts
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let bitrate = find_fact("Bitrate").and_then(|v| v.parse::<u32>().ok());
    let sample_rate = find_fact("SampleRate").and_then(|v| v.parse::<u32>().ok());
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
        position_marks,
    }
}

/// Locations to list in a playlist: the `hashes` that resolve to a location
/// which made it into `refreshed_locations`. Unresolved hashes (failed
/// downloads) are dropped.
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

pub(crate) fn export(args: RekordboxExportArgs, ctx: &Ctx) -> Result<()> {
    use std::collections::HashSet;

    let RekordboxExportArgs {
        playlist: playlists,
        output,
        format,
        replace,
    } = args;
    let library = ctx.library();
    let output = output.unwrap_or_else(|| {
        dirs::audio_dir()
            .map(|p| p.join("mdma_rekordbox"))
            .unwrap_or_else(|| std::path::PathBuf::from("./rekordbox-export/"))
    });

    let (node, node_ip) = ctx.require_node();

    // Collect hashes per playlist, or from stdin when no playlists given.
    //
    // hashes_per_playlist: ordered list of (playlist_name, ordered_hashes).
    // When stdin mode, one entry with empty name.
    let hashes_per_playlist: Vec<(String, Vec<ContentHash>)> = if !playlists.is_empty() {
        let mut result = Vec::with_capacity(playlists.len());
        for PlaylistArg(name) in &playlists {
            let pname = name.as_str();
            match library.playlist_get(name) {
                Ok(hashes) => {
                    result.push((pname.to_string(), hashes));
                }
                Err(e) => {
                    eprintln!("error: playlist '{}' not found: {}", pname, e);
                    std::process::exit(1);
                }
            }
        }
        result
    } else {
        vec![("".to_string(), read_hashes())]
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
        if !playlists.is_empty() {
            eprintln!("All specified playlists are empty.");
        } else {
            eprintln!("No hashes provided on stdin.");
            eprintln!("Usage: mdma search --artist CBL | mdma rekordbox export");
            eprintln!("       mdma rekordbox export --playlist my-set");
        }
        std::process::exit(1);
    }

    // Resolve each unique hash to full TrackInfo
    let tracks = resolve_tracks_skip_errors(&library, all_hashes);
    if tracks.is_empty() {
        eprintln!("No tracks could be resolved. Aborting.");
        std::process::exit(1);
    }

    let http = build_http_client(&node, node_ip)?;

    // Create output directory early so we can canonicalise it for stable paths
    if let Err(e) = std::fs::create_dir_all(&output) {
        return Err(color_eyre::eyre::eyre!(
            "Failed to create output directory: {}",
            e
        ));
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
                    return Err(color_eyre::eyre::eyre!(
                        "failed to parse existing rekordbox.xml: {}. Inspect the file or re-run with --replace to regenerate.",
                        err
                    ));
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
            refreshed.push(build_rekordbox_track(
                track,
                &actual_dest,
                &actual_ext,
                size,
                &facts,
            ));
        }
    }

    // Abort before touching the XML if any download failed; leave the last-good
    // XML in place so the user can re-run after fixing the problem.
    if fail_count > 0 {
        eprintln!(
            "error: {} download(s) failed — XML not written. Re-run to retry.",
            fail_count
        );
        std::process::exit(1);
    }

    if refreshed.is_empty() {
        eprintln!("No tracks available. Aborting XML generation.");
        std::process::exit(1);
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
        .map_err(|e| color_eyre::eyre::eyre!("Failed to write rekordbox.xml: {}", e))?;

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

/// Report the outcome of [`write_playlist`], warning after a replace.
fn print_write_outcome(name: &str, track_count: usize, created: bool) {
    println!("{}", format_write_outcome(name, track_count, created));
}

pub(crate) fn import(args: RekordboxImportArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::{Bpm, Key};
    use std::collections::HashMap;

    let RekordboxImportArgs {
        playlist,
        all_playlists,
        enrich,
        dry_run,
        path,
    } = args;
    let library = ctx.library();

    // 1. Read and parse XML file
    let ExistingFileArg(path) = path;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to read {}: {}", path.display(), e))?;

    let parsed = parse_xml(&content)
        .map_err(|e| color_eyre::eyre::eyre!("Failed to parse rekordbox XML: {}", e))?;

    if parsed.tracks.is_empty() {
        eprintln!("No tracks found in {}", path.display());
        return Ok(());
    }

    // 2. Create lookup and match tracks
    let lookup = LibraryTrackLookup::new(|q| library.search(q));

    let mut matched: Vec<(&RekordboxTrack, ContentHash)> = Vec::new();
    let mut ambiguous: Vec<(&RekordboxTrack, Vec<ContentHash>)> = Vec::new();
    let mut unmatched: Vec<&RekordboxTrack> = Vec::new();

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

        match track_matcher::match_track(&candidate, &lookup) {
            MatchResult::Definitive(hash, _) => {
                matched.push((track, hash));
            }
            MatchResult::Ambiguous(hashes, _) => {
                ambiguous.push((track, hashes));
            }
            MatchResult::NoMatch => {
                unmatched.push(track);
            }
        }
    }

    // A failed library search must not look like "not in library"
    lookup.check()?;

    // 3. Print summary
    println!("Matched:   {:3} tracks", matched.len());
    println!(
        "Ambiguous: {:3} tracks (need manual resolution)",
        ambiguous.len()
    );
    println!("Unmatched: {:3} tracks", unmatched.len());

    // 4. Print unmatched tracks to stderr
    if !unmatched.is_empty() {
        eprintln!();
        eprintln!("Unmatched tracks:");
        for t in &unmatched {
            let filename = rekordbox_xml::parse_location(&t.location)
                .map(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&p)
                        .to_string()
                })
                .unwrap_or_else(|| t.location.clone());
            eprintln!("  {} - {} ({})", t.artist, t.name, filename);
        }
    }

    // 5. Print ambiguous tracks to stderr
    if !ambiguous.is_empty() {
        eprintln!();
        eprintln!("Ambiguous tracks (multiple candidates):");
        for (t, hashes) in &ambiguous {
            eprintln!("  {} - {}", t.artist, t.name);
            for h in hashes.iter().take(3) {
                eprintln!("    candidate: {}", &h.as_str()[..h.as_str().len().min(16)]);
            }
        }
    }

    // Build track_id → ContentHash map for playlist resolution
    let track_id_to_hash: HashMap<u32, ContentHash> = matched
        .iter()
        .map(|(rb_track, hash)| (rb_track.track_id, hash.clone()))
        .collect();

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
                .map_err(|e| color_eyre::eyre::eyre!("Failed to write playlist: {}", e))?;
            print_write_outcome(sanitized, hashes.len(), created);
        }
    }

    // 7. Create --all-playlists
    if all_playlists {
        for rb_playlist in &parsed.playlists {
            let resolved: Vec<ContentHash> = rb_playlist
                .track_ids
                .iter()
                .filter_map(|id| track_id_to_hash.get(id).cloned())
                .collect();

            if resolved.is_empty() {
                eprintln!(
                    "Skipping playlist '{}': no matched tracks",
                    rb_playlist.name
                );
                continue;
            }

            let target = match SanitizedPlaylistName::new(&rb_playlist.name) {
                Ok(t) => t,
                Err(reason) => {
                    eprintln!("Skipping playlist '{}': {}", rb_playlist.name, reason);
                    continue;
                }
            };
            let sanitized = target.name().as_str();

            if dry_run {
                println!(
                    "[dry-run] Would create playlist '{}' (from '{}') with {} tracks",
                    sanitized,
                    rb_playlist.name,
                    resolved.len()
                );
            } else {
                match write_playlist(&library, target.name(), &resolved) {
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

        for (rb_track, hash) in &matched {
            // Fetch current track facts; on failure fall back to writing (safe default).
            let existing = library.get_track(hash).ok();

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
                        match library.write_fact(hash, MusicValue::Bpm(bpm)) {
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
            if let Some(ref tonality) = rb_track.tonality {
                if let Ok(key) = Key::from_camelot(tonality) {
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
                        match library.write_fact(hash, MusicValue::Key(key)) {
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
            if let (Some(anchor), Some(bpm_f32)) = (&rb_track.tempo_anchor, rb_track.average_bpm) {
                if let Ok(bpm) = Bpm::from_f32(bpm_f32) {
                    let new_grid = rekordbox_roundtrip::grid_from_tempo(anchor, bpm);
                    // Guard: skip if the projected beat_grid is already identical.
                    let already_current = match (
                        &new_grid,
                        existing.as_ref().and_then(|t| t.beat_grid.as_ref()),
                    ) {
                        (
                            MusicValue::BeatGrid {
                                first_beat_ms,
                                bpm: new_bpm,
                                beats_per_bar,
                            },
                            Some(existing_grid),
                        ) => {
                            existing_grid.first_beat_ms == *first_beat_ms
                                && existing_grid.bpm == *new_bpm
                                && existing_grid.beats_per_bar == *beats_per_bar
                        }
                        _ => false,
                    };

                    if already_current {
                        skipped += 1;
                    } else if dry_run {
                        println!(
                            "[dry-run] Would write BeatGrid first_beat={:.3}s bpm={} for {} - {}",
                            anchor.inizio_seconds, bpm_f32, rb_track.artist, rb_track.name
                        );
                    } else {
                        match library.write_fact(hash, new_grid) {
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
                        match library.write_fact(hash, cue) {
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
        }

        if !dry_run {
            println!(
                "Enriched {} fact(s), skipped {} (already up to date), {} error(s)",
                enriched, skipped, errors
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::playlist_locations;
    use library_ipc_client::ContentHash;
    use pretty_assertions::assert_eq;

    // ── argv parsing ─────────────────────────────────────────────────────────────

    use crate::test_support::{parses, usage_error};
    use crate::Commands;

    fn rekordbox_command(argv: &[&str]) -> super::Command {
        match parses(argv) {
            Commands::Rekordbox { command } => command,
            other => panic!("not rekordbox: {other:?}"),
        }
    }

    #[test]
    fn export_playlist_names_are_validated_at_parse_time() {
        for bad in ["has space", "a/b", ""] {
            let err = usage_error(&["rekordbox", "export", "--playlist", bad]);
            assert!(err.to_string().contains("--playlist"), "{bad:?}: {err}");
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
        match rekordbox_command(&[
            "rekordbox",
            "export",
            "--playlist",
            "set-1",
            "--playlist",
            "set_2",
        ]) {
            super::Command::Export { playlist, .. } => {
                let names: Vec<&str> = playlist.iter().map(|p| p.0.as_str()).collect();
                assert_eq!(names, ["set-1", "set_2"]);
            }
            other => panic!("not export: {other:?}"),
        }
    }

    #[test]
    fn import_rejects_a_missing_file() {
        let err = usage_error(&["rekordbox", "import", "/definitely/not/here.xml"]);
        assert!(err.to_string().contains("does not exist"), "message: {err}");
    }

    #[test]
    fn import_rejects_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = usage_error(&["rekordbox", "import", dir.path().to_str().unwrap()]);
        assert!(err.to_string().contains("is not a file"), "message: {err}");
    }

    #[test]
    fn import_playlist_that_sanitizes_to_nothing_is_rejected() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        for bad in ["", "///", " "] {
            let err = usage_error(&["rekordbox", "import", path, "--playlist", bad]);
            assert!(
                err.to_string().contains("name sanitizes to empty"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn import_accepts_existing_file_and_sanitizes_playlist() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        match rekordbox_command(&["rekordbox", "import", path, "--playlist", "My Set!"]) {
            super::Command::Import {
                path: p, playlist, ..
            } => {
                assert_eq!(p.0, file.path());
                let playlist = playlist.unwrap();
                assert_eq!(
                    playlist.change_note().as_deref(),
                    Some("Note: playlist name sanitized from 'My Set!' to 'My-Set'")
                );
                assert_eq!(playlist.name().as_str(), "My-Set");
            }
            other => panic!("not import: {other:?}"),
        }
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
