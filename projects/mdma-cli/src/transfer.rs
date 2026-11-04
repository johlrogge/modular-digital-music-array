use crate::ctx::Ctx;
use crate::stdin::{parse_hash_from_line, resolve_tracks_skip_errors};
use color_eyre::Result;
use library_ipc_client::TrackInfo;
use mdma_client::IngestSource;
use std::path::Path;
use track_formatter::short_hash;

#[derive(clap::Args, Debug)]
pub(crate) struct UploadArgs {
    /// Path to file (audio file or ZIP archive)
    file: std::path::PathBuf,

    /// SSH key for SCP transfer
    #[arg(long, default_value = "~/.ssh/mdma_pi", env = "MDMA_SSH_KEY")]
    ssh_key: String,

    /// SSH user on the Pi
    #[arg(long, default_value = "mdma", env = "MDMA_SSH_USER")]
    ssh_user: String,

    /// Remote inbox directory
    #[arg(long, default_value = "/music/inbox/", env = "MDMA_INBOX_DIR")]
    inbox_dir: String,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ExportArgs {
    /// Output format for ALL tracks (original, aiff or wav). Default: original.
    /// Conflicts with --lossless-format and --lossy-format.
    #[arg(long, value_enum, conflicts_with_all = ["lossless_format", "lossy_format"])]
    format: Option<ExportFormat>,

    /// Target format for lossless sources (flac, wav, aiff).
    /// Lossy sources pass through unchanged.
    #[arg(long, value_enum)]
    lossless_format: Option<ExportFormat>,

    /// Target format for lossy sources (mp3, ogg, opus).
    /// Lossless sources pass through unchanged.
    #[arg(long, value_enum)]
    lossy_format: Option<ExportFormat>,

    /// Output directory (created if it doesn't exist)
    #[arg(long, default_value = "./export/")]
    output: std::path::PathBuf,
}

/// Which export format applies to which tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormats {
    /// `--format` (or nothing at all): one format for every track.
    All(ExportFormat),
    /// `--lossless-format` / `--lossy-format`: per source category, `None`
    /// passes that category through unchanged.
    PerCategory {
        lossless: Option<ExportFormat>,
        lossy: Option<ExportFormat>,
    },
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// SCP a local file to a remote destination.
fn scp_file(
    local_path: &Path,
    remote_user: &str,
    remote_host: &str,
    remote_dir: &str,
    ssh_key: &Path,
) -> Result<()> {
    let dest = format!("{}@{}:{}", remote_user, remote_host, remote_dir);
    let status = std::process::Command::new("scp")
        .args([
            "-i",
            &ssh_key.to_string_lossy(),
            "-o",
            "StrictHostKeyChecking=no",
            "-4",
            &local_path.to_string_lossy(),
            &dest,
        ])
        .status()?;

    if !status.success() {
        eprintln!(
            "SCP failed for {}",
            local_path.file_name().unwrap_or_default().to_string_lossy()
        );
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) fn upload(args: UploadArgs, ctx: &Ctx) -> Result<()> {
    use library_ipc_client::InboxPath;

    let UploadArgs {
        file,
        ssh_key,
        ssh_user,
        inbox_dir,
    } = args;

    // 1. Validate file exists
    if !file.exists() {
        eprintln!("File not found: {}", file.display());
        std::process::exit(1);
    }

    // 2. Host to scp to: the resolved node, by IP when known
    let hostname = match ctx.remote_host() {
        Some((host, ip)) => ip.map_or(host, |ip| ip.to_string()),
        None => {
            eprintln!("Upload requires --node or MDMA_NODE to determine the Pi hostname");
            std::process::exit(1);
        }
    };

    let key_path = expand_tilde(&ssh_key);

    // 3. Detect file type
    let file_type = inbox_utils::detect_file_type(&file);

    // 4. Collect audio files to upload
    let temp_dir = tempfile::tempdir()?;
    let audio_files: Vec<std::path::PathBuf> = match file_type {
        Some("zip") => {
            println!("Extracting ZIP archive...");
            let extracted = inbox_utils::extract_zip(&file, temp_dir.path())?;
            if extracted.is_empty() {
                eprintln!("No audio files found in ZIP archive");
                std::process::exit(1);
            }
            println!("Found {} audio file(s)", extracted.len());
            extracted
        }
        Some("flac" | "mp3" | "wav" | "aiff") => {
            vec![file]
        }
        _ => {
            eprintln!(
                "Unsupported file type. Expected audio file (FLAC, MP3, WAV, AIFF) or ZIP archive."
            );
            std::process::exit(1);
        }
    };

    // 5. SCP each file to the Pi's inbox
    for audio_file in &audio_files {
        let filename = audio_file.file_name().unwrap_or_default().to_string_lossy();
        println!("Uploading: {}", filename);
        scp_file(audio_file, &ssh_user, &hostname, &inbox_dir, &key_path)?;
    }

    // 6. Ingest each file via the library service
    let lib = ctx.library();
    let mut success_count = 0;
    let mut fail_count = 0;

    for audio_file in &audio_files {
        let filename = audio_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let inbox_path = match InboxPath::new(&filename) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  FAIL: {} - invalid inbox path: {}", filename, e);
                fail_count += 1;
                continue;
            }
        };

        match lib.ingest_file_with_source(&inbox_path, Some(IngestSource::Upload)) {
            Ok(mdma_client::IngestResult::Success { hash, .. }) => {
                let hash_str = hash
                    .as_ref()
                    .map(|h| short_hash(h).to_string())
                    .unwrap_or_default();
                println!("  OK: {} -> {}", filename, hash_str);
                success_count += 1;
            }
            Ok(mdma_client::IngestResult::Failure { message }) => {
                eprintln!("  FAIL: {} - {}", filename, message);
                fail_count += 1;
            }
            Err(e) => {
                eprintln!("  FAIL: {} - {}", filename, e);
                fail_count += 1;
            }
        }
    }

    println!();
    println!("Done: {} succeeded, {} failed", success_count, fail_count);

    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the export URL for a track given an MDMA node hostname.
fn export_url_from_node(node: &str, hash: &str, format: &str) -> String {
    format!("http://{}/export/{}?format={}", node, hash, format)
}

/// Sanitize a single path component by replacing unsafe filesystem characters.
fn sanitize_path_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

/// Budget, in bytes, for the length of the *encoded rekordbox `Location`*
/// (`file://localhost` + percent-encoded, NFD-normalized absolute path) of a
/// NEW export entry.
///
/// rekordbox on macOS rejects paths of about 256 bytes (observed by Joakim:
/// one over-long track name made rekordbox reject the whole XML and halt the
/// import). Two things are unknown, and we are conservative about both:
///
/// 1. Whether the ~256 limit applies to the decoded path or to the
///    percent-encoded Location. We measure the encoded Location, which is
///    never shorter than the decoded path.
/// 2. Whether macOS measures the NFD form (HFS+/APFS-style decomposition) or
///    the NFC form we write. We measure NFD, which is never shorter than NFC.
///
/// The `file://localhost` prefix counts towards the budget too. Existing
/// entries are grandfathered and never shortened (see
/// [`export_dest_path_checked`]), so this only affects new tracks.
pub(crate) const MAX_LOCATION_URI_BYTES: usize = 250;

/// Secondary guard: maximum cost of a directory name (macOS/Windows/Linux
/// limit is 255 bytes; macOS stores names NFD-decomposed, so leave headroom).
const MAX_DIR_NAME_BYTES: usize = 120;

/// Secondary guard: maximum cost of a file name including the extension.
const MAX_FILE_NAME_BYTES: usize = 180;

/// Smallest cost a component is shortened to when spending the path budget.
const MIN_COMPONENT_BYTES: usize = 16;

/// Length of the rekordbox Location URI for `s` taken as an absolute path,
/// measured over its NFD form. Encoding rules come from
/// [`rekordbox_xml::path_to_file_uri`], not duplicated here.
fn location_cost(s: &str) -> usize {
    use unicode_normalization::UnicodeNormalization;
    let nfd: String = s.nfd().collect();
    rekordbox_xml::path_to_file_uri(std::path::Path::new(&nfd)).len()
}

/// Exact byte cost of a path or path component inside the rekordbox Location
/// (NFD, percent-encoded, without the `file://localhost` prefix). Additive
/// over concatenation, so it works per component and per character.
pub(crate) fn path_cost(s: &str) -> usize {
    location_cost(s) - location_cost("")
}

/// File extension of an exported track: fixed for converted formats, derived
/// from the blob path for `Original`.
pub(crate) fn export_extension(format: &ExportFormat, track: &TrackInfo) -> String {
    match format.static_extension() {
        Some(fixed) => fixed.to_string(),
        None => track
            .blob_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_lowercase(),
    }
}

/// Normalised key for comparing Location URIs: NFD and lowercase. Existing
/// Locations come from `canonicalize()`, which on HFS+ returns NFD names,
/// while track metadata is NFC; case is folded as in `plan_export`.
fn location_key(uri: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    uri.nfd().collect::<String>().to_lowercase()
}

/// Set of [`location_key`]s of an existing export, for grandfathering.
pub(crate) fn existing_location_set(
    existing: Option<&rekordbox_xml::RekordboxLibrary>,
) -> std::collections::HashSet<String> {
    existing
        .map(|lib| {
            lib.tracks
                .iter()
                .map(|t| location_key(&t.location))
                .collect()
        })
        .unwrap_or_default()
}

/// Stable FNV-1a hash used to disambiguate truncated names.
fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// Shorten `s` so its cost is at most `max_cost` (but never below
/// [`MIN_COMPONENT_BYTES`]). Strings that already fit are returned unchanged.
/// Truncation happens on a char boundary, trailing spaces/dots are trimmed
/// (Windows rejects them) and a `~<hash of the full name>` suffix is appended
/// so that two different long names never collapse onto the same path.
fn shorten_component(s: &str, max_cost: usize) -> String {
    if path_cost(s) <= max_cost {
        return s.to_string();
    }
    let suffix = format!("~{:08x}", fnv1a(s));
    let budget = max_cost
        .max(MIN_COMPONENT_BYTES)
        .saturating_sub(suffix.len());
    let mut used = 0;
    let mut kept = String::new();
    for c in s.chars() {
        let cost = path_cost(c.encode_utf8(&mut [0; 4]));
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(c);
    }
    let trimmed = kept.trim_end_matches([' ', '.']);
    format!("{trimmed}{suffix}")
}

/// Build the destination path for an exported track and report whether it
/// fits [`MAX_LOCATION_URI_BYTES`].
///
/// Layout: `<output>/<artist>/<album>/<title>.<ext>`
///
/// Components are sanitized first. If the resulting unshortened path is
/// already an entry of the existing export (`existing_locations`: lowercased
/// Location URIs), it is returned as-is and reported as fitting: renaming a
/// previously exported file would make rekordbox see a new track and lose its
/// cues. Otherwise components are shortened: per-component caps, then (if the
/// whole path is still too long) title first, album, artist.
/// Paths within all limits are returned byte-identical to the unlimited form.
pub(crate) fn export_dest_path_checked(
    output: &std::path::Path,
    track: &TrackInfo,
    ext: &str,
    existing_locations: &std::collections::HashSet<String>,
) -> (std::path::PathBuf, bool) {
    let mut artist = sanitize_path_component(track.artist.as_deref().unwrap_or("Unknown Artist"));
    let mut album = sanitize_path_component(track.album.as_deref().unwrap_or("Unknown Album"));
    let mut title = sanitize_path_component(track.title.as_deref().unwrap_or("Unknown"));
    let dot_ext = format!(".{ext}");
    let ext_cost = path_cost(&dot_ext);

    let unshortened = output
        .join(&artist)
        .join(&album)
        .join(format!("{title}{dot_ext}"));
    let unshortened_location = rekordbox_xml::path_to_file_uri(&unshortened);
    let unshortened_location = location_key(&unshortened_location);
    if existing_locations.contains(&unshortened_location) {
        return (unshortened, true);
    }

    artist = shorten_component(&artist, MAX_DIR_NAME_BYTES);
    album = shorten_component(&album, MAX_DIR_NAME_BYTES);
    title = shorten_component(&title, MAX_FILE_NAME_BYTES.saturating_sub(ext_cost));

    let build = |artist: &str, album: &str, title: &str| {
        output
            .join(artist)
            .join(album)
            .join(format!("{title}{dot_ext}"))
    };
    let total = |p: &std::path::Path| location_cost(&p.to_string_lossy());

    // Spend the whole-path budget: title first, then album, then artist.
    // A name shortened twice is re-hashed from its first shortened form, which
    // embeds the first hash, so distinct originals stay distinct.
    for which in 0..3 {
        let over = total(&build(&artist, &album, &title)).saturating_sub(MAX_LOCATION_URI_BYTES);
        if over == 0 {
            break;
        }
        let field = match which {
            0 => &mut title,
            1 => &mut album,
            _ => &mut artist,
        };
        let cost = path_cost(field);
        let target = cost.saturating_sub(over).max(MIN_COMPONENT_BYTES);
        if target < cost {
            *field = shorten_component(field, target);
        }
    }

    let path = build(&artist, &album, &title);
    let fits = total(&path) <= MAX_LOCATION_URI_BYTES;
    (path, fits)
}

/// Build the destination path for an exported track with no existing export
/// to preserve (see [`export_dest_path_checked`]).
///
/// Missing metadata falls back to `"Unknown Artist"`, `"Unknown Album"`, `"Unknown"`.
fn export_dest_path(output: &std::path::Path, track: &TrackInfo, ext: &str) -> std::path::PathBuf {
    export_dest_path_checked(output, track, ext, &std::collections::HashSet::new()).0
}

/// Build the shared blocking HTTP client used by export handlers.
///
/// Pins the node's known IPv4 address via `resolve_to_addrs`; without one,
/// resolves `node` (using avahi for `.local` hostnames) so the request succeeds
/// even in environments where mDNS is not wired into the system resolver (e.g.
/// Nix-glibc).  HTTP on port 80 is used; the URL hostname is kept as-is so
/// virtual-host routing on the server side is unaffected.
pub(crate) fn build_http_client(
    node: &str,
    known_ip: Option<std::net::Ipv4Addr>,
) -> Result<reqwest::blocking::Client> {
    use std::net::{IpAddr, SocketAddr};

    let ip_str = match known_ip {
        Some(ip) => ip.to_string(),
        None => nng_transport::resolve_hostname_to_ipv4(node).map_err(|e| {
            color_eyre::eyre::eyre!(
                "Failed to resolve '{}' to an IP address: {}. \
                 Try passing an explicit IP with --node <ip>.",
                node,
                e
            )
        })?,
    };
    let ip: IpAddr = ip_str.parse().map_err(|e| {
        color_eyre::eyre::eyre!("Resolved address '{}' is not a valid IP: {}", ip_str, e)
    })?;
    let addr = SocketAddr::new(ip, 80);

    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .resolve_to_addrs(node, &[addr])
        .build()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to build HTTP client: {}", e))
}

/// Outcome of a single track download attempt.
pub(crate) enum DownloadOutcome {
    Success {
        dest: std::path::PathBuf,
        size: u64,
        ext: String,
    },
    Failure {
        error: String,
    },
}

/// Result of a single track download attempt.
///
/// Carries enough information for both the plain export summary and the
/// Rekordbox XML builder.
pub(crate) struct DownloadAttempt {
    pub(crate) track: TrackInfo,
    pub(crate) hash: String,
    pub(crate) outcome: DownloadOutcome,
}

/// Download a list of tracks to `output`, resolving each track's format via
/// `resolve_format`.  Progress is printed to stderr.  Returns one
/// `DownloadAttempt` per input track, in order.
///
/// `dest_for(track, ext)` gives the destination path of a track, so callers
/// that planned destinations up front (rekordbox export) download to exactly
/// the planned path.
pub(crate) fn download_tracks(
    node: &str,
    tracks: &[TrackInfo],
    http: &reqwest::blocking::Client,
    resolve_format: impl Fn(&TrackInfo) -> ExportFormat,
    dest_for: impl Fn(&TrackInfo, &str) -> std::path::PathBuf,
) -> Vec<DownloadAttempt> {
    use std::io::Write;

    let total = tracks.len();
    let mut results: Vec<DownloadAttempt> = Vec::with_capacity(total);

    for (idx, track) in tracks.iter().enumerate() {
        let full_hash = track.content_hash.as_str();
        let resolved_format = resolve_format(track);
        let format_param = resolved_format.format_param();
        let url = export_url_from_node(node, full_hash, format_param);

        eprint!("[{}/{}] Downloading {} ... ", idx + 1, total, full_hash);
        let _ = std::io::stderr().flush();

        macro_rules! fail {
            ($msg:expr) => {{
                eprintln!("FAILED ({})", $msg);
                results.push(DownloadAttempt {
                    track: track.clone(),
                    hash: full_hash.to_string(),
                    outcome: DownloadOutcome::Failure {
                        error: $msg.to_string(),
                    },
                });
                continue;
            }};
        }

        let response = match http.get(&url).send() {
            Ok(r) => r,
            Err(e) => fail!(e.to_string()),
        };

        if !response.status().is_success() {
            fail!(format!("HTTP {}", response.status()));
        }

        // Determine extension: fixed for converted formats, derived from blob_path for Original.
        let ext = export_extension(&resolved_format, track);

        let dest = dest_for(track, &ext);

        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                fail!(format!("mkdir: {}", e));
            }
        }

        let body = match response.bytes() {
            Ok(b) => b,
            Err(e) => fail!(format!("read error: {}", e)),
        };

        if let Err(e) = std::fs::write(&dest, &body) {
            fail!(format!("write error: {}", e));
        }

        let size = body.len() as u64;
        eprintln!("OK  {} ({} bytes)", dest.display(), size);
        results.push(DownloadAttempt {
            track: track.clone(),
            hash: full_hash.to_string(),
            outcome: DownloadOutcome::Success {
                dest,
                size,
                ext: ext.to_string(),
            },
        });
    }

    results
}

/// Resolve the export format for a track based on its source extension and CLI flags.
///
/// Priority:
/// 1. `--format` — applies to all tracks unconditionally.
/// 2. `--lossless-format` / `--lossy-format` — applied per source category.
/// 3. No flags — defaults to `ExportFormat::Original` (pass-through).
fn resolve_export_format(blob_path: Option<&str>, formats: &ExportFormats) -> ExportFormat {
    // If --format is given, use it for everything.
    let (lossless_format, lossy_format) = match formats {
        ExportFormats::All(fmt) => return *fmt,
        ExportFormats::PerCategory { lossless, lossy } => (lossless, lossy),
    };

    // If neither category flag is given, default to Original.
    if lossless_format.is_none() && lossy_format.is_none() {
        return ExportFormat::Original;
    }

    // Classify the source file by extension.
    let ext = blob_path
        .and_then(|p| std::path::Path::new(p).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match audio_transcoder::FormatCategory::from_extension(ext) {
        Some(audio_transcoder::FormatCategory::Lossless) => {
            lossless_format.unwrap_or(ExportFormat::Original)
        }
        Some(audio_transcoder::FormatCategory::Lossy) => {
            lossy_format.unwrap_or(ExportFormat::Original)
        }
        None => ExportFormat::Original,
    }
}

pub(crate) fn export(args: ExportArgs, ctx: &Ctx) -> Result<()> {
    use std::io::BufRead;

    let ExportArgs {
        format,
        lossless_format,
        lossy_format,
        output,
    } = args;
    let library = ctx.library();
    let formats = match format {
        Some(fmt) => ExportFormats::All(fmt),
        None => ExportFormats::PerCategory {
            lossless: lossless_format,
            lossy: lossy_format,
        },
    };

    let (node, node_ip) = ctx.require_node();

    // Read hashes from stdin
    let raw_hashes: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_hash_from_line(&line))
        .collect();

    if raw_hashes.is_empty() {
        eprintln!("No hashes provided on stdin.");
        eprintln!("Usage: mdma search --artist CBL | mdma export");
        std::process::exit(1);
    }

    // Resolve each hash (short or full) to a full TrackInfo via the library
    let tracks = resolve_tracks_skip_errors(&library, raw_hashes);

    if tracks.is_empty() {
        eprintln!("No tracks could be resolved. Aborting.");
        std::process::exit(1);
    }

    let http = build_http_client(&node, node_ip)?;

    let results = download_tracks(
        &node,
        &tracks,
        &http,
        |track| resolve_export_format(track.blob_path.as_deref(), &formats),
        |track, ext| export_dest_path(&output, track, ext),
    );

    // Summary
    let success_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Success { .. }))
        .count();
    let total_bytes: u64 = results
        .iter()
        .filter_map(|r| {
            if let DownloadOutcome::Success { size, .. } = &r.outcome {
                Some(*size)
            } else {
                None
            }
        })
        .sum();
    let fail_count = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Failure { .. }))
        .count();
    eprintln!();
    eprintln!(
        "Done: {} exported ({} bytes), {} failed",
        success_count, total_bytes, fail_count
    );

    if fail_count > 0 {
        eprintln!();
        eprintln!("Failed tracks:");
        for (r, error) in results.iter().filter_map(|r| {
            if let DownloadOutcome::Failure { error } = &r.outcome {
                Some((r, error.as_str()))
            } else {
                None
            }
        }) {
            eprintln!("  {} — {}", r.hash, error);
        }
        std::process::exit(1);
    }

    Ok(())
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    /// Transfer the file without conversion (default)
    Original,
    Aiff,
    Wav,
}

impl ExportFormat {
    /// Return the fixed file extension for converted formats, or `None` for `Original`.
    pub(crate) fn static_extension(&self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Aiff => Some("aiff"),
            Self::Wav => Some("wav"),
        }
    }

    /// Format string to pass in the `?format=` query parameter.
    pub(crate) fn format_param(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Aiff => "aiff",
            Self::Wav => "wav",
        }
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.format_param())
    }
}

impl std::str::FromStr for ExportFormat {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "original" => Ok(Self::Original),
            "aiff" => Ok(Self::Aiff),
            "wav" => Ok(Self::Wav),
            _ => Err(format!(
                "unknown export format '{s}' (expected original, aiff or wav)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::track;

    use library_ipc_client::TrackInfo;

    use pretty_assertions::assert_eq;

    // ── Export helpers ────────────────────────────────────────────────────────

    #[test]
    fn export_url_builds_from_node_aiff() {
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "aiff");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=aiff"
        );
    }

    #[test]
    fn export_url_builds_from_node_wav() {
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "wav");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=wav"
        );
    }

    #[test]
    fn export_url_builds_with_original_format() {
        // Original passes format=original in the query string
        let url = export_url_from_node("mdma-909.local", "sha256:abcd1234", "original");
        assert_eq!(
            url,
            "http://mdma-909.local/export/sha256:abcd1234?format=original"
        );
    }

    // ── export_dest_path builds artist/album/title hierarchy ─────────────────

    fn make_track_full(artist: &str, album: &str, title: &str, blob_path: &str) -> TrackInfo {
        use library_ipc_client::DurationSeconds;
        TrackInfo {
            title: Some(title.to_string()),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            duration: Some(DurationSeconds::new(300)),
            blob_path: Some(blob_path.to_string()),
            ..track("sha256:aa000001")
        }
    }

    #[test]
    fn export_dest_path_uses_artist_album_title() {
        let track = make_track_full(
            "Carbon Based Lifeforms",
            "Twentythree",
            "Polyrytmi",
            "ab/abc123.flac",
        );
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "flac");
        assert_eq!(
            path,
            std::path::PathBuf::from(
                "/tmp/export/Carbon Based Lifeforms/Twentythree/Polyrytmi.flac"
            )
        );
    }

    #[test]
    fn export_dest_path_sanitizes_slash_in_artist() {
        let track = make_track_full("AC/DC", "Back in Black", "Thunderstruck", "ab/abc123.mp3");
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "mp3");
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/export/AC_DC/Back in Black/Thunderstruck.mp3")
        );
    }

    #[test]
    fn export_dest_path_uses_fallbacks_for_missing_metadata() {
        use library_ipc_client::DurationSeconds;
        let track = TrackInfo {
            duration: Some(DurationSeconds::new(120)),
            blob_path: Some("ab/abc123.flac".to_string()),
            ..track("sha256:deadbeef")
        };
        let output = std::path::Path::new("/tmp/export");
        let path = export_dest_path(output, &track, "flac");
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/export/Unknown Artist/Unknown Album/Unknown.flac")
        );
    }

    #[test]
    fn export_dest_path_sanitizes_all_unsafe_chars() {
        let track = make_track_full(
            "Art:ist*Name",
            "Album?<>|Name",
            r#"Track "Remix""#,
            "ab/abc123.aiff",
        );
        let output = std::path::Path::new("/out");
        let path = export_dest_path(output, &track, "aiff");
        assert_eq!(
            path,
            std::path::PathBuf::from("/out/Art_ist_Name/Album____Name/Track _Remix_.aiff")
        );
    }

    // ── export_dest_path length limits ───────────────────────────────────────

    fn rel_components(output: &std::path::Path, path: &std::path::Path) -> Vec<String> {
        path.strip_prefix(output)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_str().unwrap().to_string())
            .collect()
    }

    fn assert_clean_name(name: &str) {
        assert!(!name.ends_with('.'), "trailing dot in {name:?}");
        assert!(!name.ends_with(' '), "trailing space in {name:?}");
    }

    #[test]
    fn export_dest_path_caps_long_ascii_title_and_keeps_extension() {
        let title = "a".repeat(300);
        let track = make_track_full("Artist", "Album", &title, "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let comps = rel_components(out, &path);
        assert_eq!(comps.len(), 3);
        assert!(comps[2].len() <= 180, "file name {} bytes", comps[2].len());
        assert!(comps[2].ends_with(".flac"));
        assert_clean_name(&comps[2]);
    }

    #[test]
    fn export_dest_path_truncates_multibyte_on_char_boundary() {
        for ch in ["å", "ä", "漢"] {
            let title = ch.repeat(300);
            let track = make_track_full("Artist", "Album", &title, "x.mp3");
            let out = std::path::Path::new("/out");
            let path = export_dest_path(out, &track, "mp3");
            let comps = rel_components(out, &path);
            assert!(comps[2].len() <= 180);
            assert!(comps[2].ends_with(".mp3"));
            assert!(comps[2].starts_with(ch));
        }
    }

    #[test]
    fn export_dest_path_caps_long_directories() {
        let long = "b".repeat(300);
        let track = make_track_full(&long, &long, "T", "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let comps = rel_components(out, &path);
        assert!(comps[0].len() <= 120);
        assert!(comps[1].len() <= 120);
        assert_eq!(comps[2], "T.flac");
    }

    #[test]
    fn export_dest_path_total_relative_length_is_bounded() {
        let long = "c".repeat(300);
        let track = make_track_full(&long, &long, &long, "x.flac");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        let full = path.to_str().unwrap();
        assert!(
            location_cost(full) <= MAX_LOCATION_URI_BYTES,
            "path {} bytes",
            location_cost(full)
        );
        assert!(full.ends_with(".flac"));
    }

    #[test]
    fn export_dest_path_long_output_dir_shrinks_title_first() {
        let out_dir = format!("/{}", "o".repeat(150));
        let out = std::path::Path::new(&out_dir);
        let track = make_track_full("Artist Name", "Album Name", &"t".repeat(100), "x");
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(fits);
        let full = path.to_str().unwrap();
        assert!(location_cost(full) <= MAX_LOCATION_URI_BYTES);
        let comps = rel_components(out, &path);
        assert_eq!(comps[0], "Artist Name");
        assert_eq!(comps[1], "Album Name");
        assert!(comps[2].len() < 100 && comps[2].ends_with(".flac"));
    }

    #[test]
    fn export_dest_path_reports_when_output_dir_leaves_no_room() {
        let out_dir = format!("/{}", "o".repeat(240));
        let out = std::path::Path::new(&out_dir);
        let track = make_track_full("Artist", "Album", "Title", "x");
        let (_, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(!fits);
    }

    fn no_existing() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    #[test]
    fn export_dest_path_counts_non_ascii_by_utf8_bytes() {
        // 60 CJK chars = 180 UTF-8 bytes (CJK does not decompose); with
        // ".flac" that exceeds the 180-byte file name cap, so it is shortened.
        let title = "漢".repeat(60);
        let track = make_track_full("A", "B", &title, "x");
        let out = std::path::Path::new("/out");
        let path = export_dest_path(out, &track, "flac");
        assert_ne!(path, out.join("A").join("B").join(format!("{title}.flac")));
        assert!(location_cost(path.to_str().unwrap()) <= MAX_LOCATION_URI_BYTES);
    }

    #[test]
    fn path_cost_measures_nfd_exactly() {
        // é (precomposed) is 2 bytes in NFC but 3 in NFD (e + U+0301).
        assert_eq!(path_cost("é"), 3);
        // Hangul syllable: 3 bytes NFC, 3 jamo x 3 bytes = 9 in NFD.
        assert_eq!(path_cost("한"), 9);
        assert_eq!(path_cost("abc"), 3);
    }

    #[test]
    fn export_dest_path_latin_accents_under_nfd_budget_are_not_shortened() {
        // 35 x é = 105 bytes NFD per component. Whole Location is
        // 16 + 4 + 2 + 1 + 105 + 1 + 105 + 5 = 239 <= 250. The old 3x rule
        // (210 bytes per component) would have renamed this.
        let accented = "é".repeat(35);
        let track = make_track_full("A", &accented, &accented, "x");
        let out = std::path::Path::new("/out");
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert!(fits);
        assert_eq!(
            path,
            out.join("A")
                .join(&accented)
                .join(format!("{accented}.flac"))
        );
    }

    #[test]
    fn export_dest_path_counts_spaces_as_encoded_bytes() {
        // Each component: 20 x "x " + "y" = 41 raw bytes, 81 encoded (space is
        // %20). Raw path is ~135 bytes, but the encoded Location is 271 > 250.
        let part = format!("{}y", "x ".repeat(20));
        let track = make_track_full(&part, &part, &part, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out.join(&part).join(&part).join(format!("{part}.flac"));
        let (path, fits) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert_ne!(path, unshortened);
        assert!(fits);
        assert!(
            rekordbox_xml::path_to_file_uri(&path).len() <= MAX_LOCATION_URI_BYTES,
            "encoded location too long"
        );
    }

    #[test]
    fn export_dest_path_grandfathers_nfd_location_for_nfc_metadata() {
        use unicode_normalization::UnicodeNormalization;
        // Over-budget accented path; the existing Location was written from an
        // HFS+ realpath, i.e. NFD, while the track metadata is NFC.
        let title = "é".repeat(100);
        let track = make_track_full("Artist", "Album", &title, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out
            .join("Artist")
            .join("Album")
            .join(format!("{title}.flac"));
        let nfd_uri: String = rekordbox_xml::path_to_file_uri(&unshortened)
            .nfd()
            .collect();
        assert_ne!(nfd_uri, rekordbox_xml::path_to_file_uri(&unshortened));
        let existing: std::collections::HashSet<String> =
            [location_key(&nfd_uri)].into_iter().collect();
        let (kept, fits) = export_dest_path_checked(out, &track, "flac", &existing);
        assert_eq!(kept, unshortened);
        assert!(fits);
    }

    #[test]
    fn export_dest_path_grandfathers_existing_export_entry() {
        let title = "a".repeat(300);
        let track = make_track_full("Artist", "Album", &title, "x");
        let out = std::path::Path::new("/out");
        let unshortened = out
            .join("Artist")
            .join("Album")
            .join(format!("{title}.flac"));
        // Without an existing entry the path is shortened.
        let (shortened, _) = export_dest_path_checked(out, &track, "flac", &no_existing());
        assert_ne!(shortened, unshortened);
        // With an existing entry (any case) the unshortened path is kept.
        let existing: std::collections::HashSet<String> = [location_key(
            &rekordbox_xml::path_to_file_uri(&unshortened).to_uppercase(),
        )]
        .into_iter()
        .collect();
        let (kept, fits) = export_dest_path_checked(out, &track, "flac", &existing);
        assert_eq!(kept, unshortened);
        assert!(fits);
        // Another track is unaffected by that entry.
        let other = make_track_full("Artist", "Album", &"b".repeat(300), "x");
        let (other_path, _) = export_dest_path_checked(out, &other, "flac", &existing);
        assert_ne!(
            other_path,
            out.join("Artist")
                .join("Album")
                .join(format!("{}.flac", "b".repeat(300)))
        );
    }

    #[test]
    fn export_dest_path_trims_trailing_dots_and_spaces_after_truncation() {
        // Place a space/dot exactly at the truncation point.
        let out = std::path::Path::new("/out");
        for n in 30..60 {
            let t = format!("{}{}{}", "d".repeat(100 + n), ". ".repeat(40), "e");
            let track2 = make_track_full(&format!("{}. ", "f".repeat(100 + n)), "Album", &t, "x");
            let comps = rel_components(out, &export_dest_path(out, &track2, "flac"));
            assert_clean_name(&comps[0]);
            let stem = comps[2].strip_suffix(".flac").unwrap();
            assert_clean_name(stem);
        }
    }

    #[test]
    fn export_dest_path_truncated_collisions_are_disambiguated() {
        let base = "g".repeat(400);
        let t1 = make_track_full("Artist", "Album", &format!("{base}one"), "x");
        let t2 = make_track_full("Artist", "Album", &format!("{base}two"), "x");
        let out = std::path::Path::new("/out");
        let p1 = export_dest_path(out, &t1, "flac");
        let p2 = export_dest_path(out, &t2, "flac");
        assert_ne!(p1, p2);
        // Deterministic across calls.
        assert_eq!(p1, export_dest_path(out, &t1, "flac"));
    }

    #[test]
    fn export_dest_path_short_paths_are_unchanged() {
        let track = make_track_full("Björk", "Homogenic", "Jóga ", "x.flac");
        let out = std::path::Path::new("/out");
        assert_eq!(
            export_dest_path(out, &track, "flac"),
            std::path::PathBuf::from("/out/Björk/Homogenic/Jóga .flac")
        );
        // Exactly at the file-name limit stays untouched.
        let title = "h".repeat(180 - ".flac".len());
        let track = make_track_full("A", "B", &title, "x");
        let p = export_dest_path(out, &track, "flac");
        assert_eq!(p, out.join("A").join("B").join(format!("{title}.flac")));
    }

    // ── resolve_export_format ────────────────────────────────────────────────

    fn per_category(lossless: Option<ExportFormat>, lossy: Option<ExportFormat>) -> ExportFormats {
        ExportFormats::PerCategory { lossless, lossy }
    }

    #[test]
    fn resolve_format_with_format_flag_overrides_everything() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &ExportFormats::All(ExportFormat::Wav),
        );
        assert_eq!(result, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_no_flags_defaults_to_original() {
        let result = resolve_export_format(Some("ab/abc123.flac"), &per_category(None, None));
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_lossless_flag_converts_flac() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &per_category(Some(ExportFormat::Aiff), None),
        );
        assert_eq!(result, ExportFormat::Aiff);
    }

    #[test]
    fn resolve_format_lossless_flag_passes_through_mp3() {
        let result = resolve_export_format(
            Some("ab/abc123.mp3"),
            &per_category(Some(ExportFormat::Aiff), None),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_lossy_flag_converts_mp3() {
        let result = resolve_export_format(
            Some("ab/abc123.mp3"),
            &per_category(None, Some(ExportFormat::Wav)),
        );
        assert_eq!(result, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_lossy_flag_passes_through_flac() {
        let result = resolve_export_format(
            Some("ab/abc123.flac"),
            &per_category(None, Some(ExportFormat::Wav)),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_both_category_flags() {
        // lossless → aiff, lossy → wav
        let both = per_category(Some(ExportFormat::Aiff), Some(ExportFormat::Wav));
        let lossless = resolve_export_format(Some("ab/abc123.flac"), &both);
        assert_eq!(lossless, ExportFormat::Aiff);

        let lossy = resolve_export_format(Some("ab/abc123.mp3"), &both);
        assert_eq!(lossy, ExportFormat::Wav);
    }

    #[test]
    fn resolve_format_unknown_extension_passes_through() {
        let result = resolve_export_format(
            Some("ab/abc123.txt"),
            &per_category(Some(ExportFormat::Aiff), Some(ExportFormat::Wav)),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    #[test]
    fn resolve_format_no_blob_path_with_category_flags_passes_through() {
        let result = resolve_export_format(
            None,
            &per_category(Some(ExportFormat::Aiff), Some(ExportFormat::Wav)),
        );
        assert_eq!(result, ExportFormat::Original);
    }

    // ── ExportFormat ──
    #[test]
    fn export_format_original_extension_returns_none() {
        // Original has no fixed extension — caller derives it from blob_path
        assert!(ExportFormat::Original.static_extension().is_none());
    }

    #[test]
    fn export_format_aiff_extension_is_aiff() {
        assert_eq!(ExportFormat::Aiff.static_extension(), Some("aiff"));
    }

    #[test]
    fn export_format_wav_extension_is_wav() {
        assert_eq!(ExportFormat::Wav.static_extension(), Some("wav"));
    }

    #[test]
    fn export_format_parse_and_display_match_clap_value_enum() {
        use clap::ValueEnum;
        for v in ExportFormat::value_variants() {
            let name = v.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(name.parse::<ExportFormat>(), Ok(*v));
            assert_eq!(v.to_string(), name);
        }
    }

    #[test]
    fn export_format_rejects_unknown_name() {
        assert!("flac".parse::<ExportFormat>().is_err());
    }
}
