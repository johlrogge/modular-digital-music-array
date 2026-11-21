//! The verified AIFF to FLAC encoder (ADR-037).
//!
//! [`encode_aiff_to_flac`] runs `flac -8 --keep-foreign-metadata --verify`,
//! writes Vorbis comments and PICTURE blocks from the AIFF's tag, then proves
//! the result: stream properties equal, PcmHash equal, the original file
//! rebuilt byte for byte, tag facts and pictures equal. Anything less is an
//! [`EncodeError`]:
//! - [`EncodeError::Refused`]: the file cannot be converted or verified; the
//!   caller keeps the AIFF as an AIFF blob;
//! - [`EncodeError::Environment`]: the machine failed (no `flac`, disk I/O);
//!   the caller retries later.
//!
//! Home: a component of its own. `audio_transcoder` wraps ffmpeg for export, a
//! lossy path that drops what this one keeps; this one needs libFLAC, `audio_hash`
//! (the independent claxon decode) and the ingestion's tag facts, and nothing of
//! the library service, the fact store or the network.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use audio_hash::{
    pcm_hash, pcm_info, AudioHashError, Container, PcmHashError, PcmInfo, Unsupported,
    FLAC_HASHABLE_BIT_DEPTHS,
};
use library_ingestion::compute_hash;
use library_ingestion::fact_generator::tag_facts;
use music_facts::{ContentHash, FactSource, MusicValue};
use music_primitives::PcmHash;

mod flac_tool;
mod refusal;
mod tags;

#[cfg(test)]
mod tests;

pub use refusal::{EncodeError, EnvironmentError, Refusal, RefusalReason};

use flac_tool::FlacTool;

/// A FLAC that passed every acceptance check. The file is at `flac_path`, in
/// the staging directory; the caller moves it into place.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    /// `<staging>/<source hex>.flac`.
    pub flac_path: PathBuf,
    /// PcmHash of the source and of the FLAC, which are equal.
    pub pcm_hash: PcmHash,
    /// The source AIFF's ContentHash: what `EncodedFrom` records.
    pub source_content_hash: ContentHash,
    /// The FLAC's ContentHash: the blob's entity key.
    pub flac_content_hash: ContentHash,
    /// The tag facts of the source, which check 5.4 proved equal to the FLAC's
    /// (see `check_tag_facts` for the one exception). The caller writes
    /// exactly these, stamped with its own content source, without reading the
    /// tags again.
    pub tag_facts: Vec<MusicValue>,
    pub source_bytes: u64,
    pub flac_bytes: u64,
}

/// Encode with the `flac` found on PATH.
pub fn encode_aiff_to_flac(src: &Path, staging_dir: &Path) -> Result<Verified, EncodeError> {
    Encoder::new().encode(src, staging_dir)
}

/// Where the encoder is, for the test hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Stage {
    /// flac has written the file.
    Encoded,
    /// The Vorbis comments and pictures are in.
    Tagged,
}

#[cfg(test)]
type Hook = Box<dyn Fn(Stage, &Path) + Send + Sync>;

/// Bytes available to an unprivileged writer in the directory's file system.
type FreeSpace = Box<dyn Fn(&Path) -> io::Result<u64> + Send + Sync>;

/// Spare space on top of twice the source (the FLAC, the rebuilt AIFF and
/// tag rewrites), and what must still be free after a failure for it not to be
/// blamed on a full disk.
const SPACE_MARGIN: u64 = 16 * 1024 * 1024;

pub struct Encoder {
    tool: FlacTool,
    free_space: FreeSpace,
    #[cfg(test)]
    hook: Option<Hook>,
}

/// `statvfs`: free bytes for an unprivileged writer.
#[allow(clippy::unnecessary_cast)] // the field widths differ by platform
fn free_bytes(dir: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a plain
    // struct that statvfs fills in.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Uses the `flac` on PATH.
    pub fn new() -> Self {
        Self::with_tool(FlacTool::on_path())
    }

    /// Uses this `flac` binary.
    pub fn with_flac_binary(binary: impl Into<PathBuf>) -> Self {
        Self::with_tool(FlacTool::at(binary))
    }

    fn with_tool(tool: FlacTool) -> Self {
        Encoder {
            tool,
            free_space: Box::new(free_bytes),
            #[cfg(test)]
            hook: None,
        }
    }

    /// Test hook: replaces the free-space probe.
    #[cfg(test)]
    pub(crate) fn with_free_space(
        mut self,
        probe: impl Fn(&Path) -> io::Result<u64> + Send + Sync + 'static,
    ) -> Self {
        self.free_space = Box::new(probe);
        self
    }

    /// Test hook: called with the staged FLAC after each stage, to damage it.
    #[cfg(test)]
    pub(crate) fn with_hook(mut self, hook: impl Fn(Stage, &Path) + Send + Sync + 'static) -> Self {
        self.hook = Some(Box::new(hook));
        self
    }

    #[cfg(test)]
    fn after(&self, stage: Stage, flac: &Path) {
        if let Some(hook) = &self.hook {
            hook(stage, flac);
        }
    }

    #[cfg(not(test))]
    fn after(&self, _stage: Stage, _flac: &Path) {}

    /// Encode `src` into `staging_dir/<source hex>.flac` and verify it.
    ///
    /// Refusals that need no encoder (float, compressed, more than 2 channels,
    /// unsupported depth, unreadable source) come before `flac` runs. The work
    /// happens in uniquely named files and the FLAC is renamed to its final
    /// name only once verified, so encodes of the same content at the same
    /// time do not trample each other. On every path the staging directory
    /// keeps nothing but the returned FLAC.
    pub fn encode(&self, src: &Path, staging_dir: &Path) -> Result<Verified, EncodeError> {
        // Absolute paths: flac must never take a name for an option.
        let src = std::path::absolute(src).map_err(|e| {
            Refusal::new(
                RefusalReason::UnreadableSource,
                format!("{}: {e}", src.display()),
            )
        })?;
        let source = read_source(&src)?;
        self.tool.ensure_usable()?;

        let staging = std::path::absolute(staging_dir)
            .and_then(|dir| fs::create_dir_all(&dir).map(|()| dir))
            .map_err(|e| EnvironmentError::Staging(format!("{}: {e}", staging_dir.display())))?;
        let hex = source.content_hash.hex();
        let unique = unique_suffix();
        let staged = Staged {
            flac: staging.join(format!("{hex}.{unique}.encoding.flac")),
            rebuilt: staging.join(format!("{hex}.{unique}.rebuilt.aiff")),
        };
        let final_path = staging.join(format!("{hex}.flac"));

        // A full disk must not look like a bad file: check before, and again
        // when something failed.
        let need = source.bytes.saturating_mul(2).saturating_add(SPACE_MARGIN);
        let free = (self.free_space)(&staging).map_err(|e| {
            EnvironmentError::Staging(format!("free space of {}: {e}", staging.display()))
        })?;
        if free < need {
            return Err(EnvironmentError::Staging(format!(
                "not enough space: {free} bytes free in {}, need {need}",
                staging.display()
            ))
            .into());
        }

        self.encode_staged(&src, source, &staged, &final_path)
            .map_err(|e| self.blame_the_disk(e, &staging))
    }

    /// The work, in the staged files. `staged` removes what is left on failure.
    fn encode_staged(
        &self,
        src: &Path,
        source: Source,
        staged: &Staged,
        final_path: &Path,
    ) -> Result<Verified, EncodeError> {
        self.tool.encode(src, &staged.flac)?;
        self.after(Stage::Encoded, &staged.flac);
        tags::write_from(src, &staged.flac)?;
        self.after(Stage::Tagged, &staged.flac);

        accept(&self.tool, src, &source, staged)?;

        let flac_content_hash = staged_hash(&staged.flac)?;
        let flac_bytes = staged_len(&staged.flac)?;
        fs::rename(&staged.flac, final_path)
            .map_err(|e| EnvironmentError::StagedFile(format!("{}: {e}", final_path.display())))?;
        // `staged` still names the work file, which the rename has moved away:
        // dropping it removes the rebuilt AIFF and nothing else.

        Ok(Verified {
            flac_path: final_path.to_path_buf(),
            pcm_hash: source.pcm_hash,
            source_content_hash: source.content_hash,
            flac_content_hash,
            tag_facts: source.tag_facts.into_iter().map(|(v, _)| v).collect(),
            source_bytes: source.bytes,
            flac_bytes,
        })
    }

    /// A refusal that arose while writing, with the disk (nearly) full, is the
    /// disk's doing: flac failing on ENOSPC or a truncated file looks like a
    /// bad file. Report the machine instead, so the ingest is retried.
    fn blame_the_disk(&self, error: EncodeError, staging: &Path) -> EncodeError {
        let EncodeError::Refused(refusal) = &error else {
            return error;
        };
        match (self.free_space)(staging) {
            Ok(free) if free < SPACE_MARGIN => EnvironmentError::Staging(format!(
                "not enough space: {free} bytes free in {} after: {refusal}",
                staging.display()
            ))
            .into(),
            _ => error,
        }
    }
}

/// A name part that no other encode in this process or on this machine uses at
/// the same time.
fn unique_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// What is known of the source before anything is encoded.
struct Source {
    info: PcmInfo,
    pcm_hash: PcmHash,
    content_hash: ContentHash,
    tag_facts: Vec<(MusicValue, FactSource)>,
    bytes: u64,
}

/// The source of the tag facts compared in check 5.4. Both sides use it, so
/// only the facts differ.
fn comparison_source() -> FactSource {
    FactSource::new("lossless-encoder", env!("CARGO_PKG_VERSION"), "Acceptance")
}

/// Read what the acceptance needs from the source, refusing what cannot be
/// verified. Runs no external program.
fn read_source(src: &Path) -> Result<Source, Refusal> {
    let info = pcm_info(src).map_err(source_refusal)?;
    if info.container != Container::Aiff {
        return Err(Refusal::new(
            RefusalReason::UnsupportedFormat,
            format!("{:?}, not AIFF", info.container),
        ));
    }
    if !FLAC_HASHABLE_BIT_DEPTHS.contains(&info.bits_per_sample) {
        return Err(Refusal::new(
            RefusalReason::UnsupportedDepth,
            format!(
                "{}-bit (the independent decode reads {:?})",
                info.bits_per_sample, FLAC_HASHABLE_BIT_DEPTHS
            ),
        ));
    }
    let pcm_hash = pcm_hash(src).map_err(source_refusal)?;
    let tag_facts = tag_facts(src, &comparison_source())
        .map_err(|e| Refusal::new(RefusalReason::UnreadableSource, format!("tags: {e}")))?;
    let content_hash = compute_hash(src).map_err(unreadable)?;
    let bytes = fs::metadata(src).map_err(unreadable)?.len();
    Ok(Source {
        info,
        pcm_hash,
        content_hash,
        tag_facts,
        bytes,
    })
}

/// What `PcmHash` says about the source, as a reason. Exhaustive: a new
/// `Unsupported` must be placed here.
fn source_refusal(e: PcmHashError) -> Refusal {
    let reason = match &e {
        PcmHashError::Unsupported(why) => match why {
            Unsupported::MoreThanTwoChannels => RefusalReason::TooManyChannels,
            Unsupported::CompressedAiffC => RefusalReason::FloatOrCompressed,
            Unsupported::SampleSizeOver32 => RefusalReason::UnsupportedDepth,
            Unsupported::NonIntegerRate => RefusalReason::NonIntegerRate,
            Unsupported::Mp3Lossy
            | Unsupported::Mp4
            | Unsupported::StreamInfoContainer
            | Unsupported::NotAiff
            | Unsupported::WavUnknownSubFormat
            | Unsupported::WavNonPcmSubFormat
            | Unsupported::WavFloat
            | Unsupported::WavNonPcmFormat
            | Unsupported::WavContainerOver4Bytes
            | Unsupported::FlacDecoder(_) => RefusalReason::UnsupportedFormat,
        },
        PcmHashError::Container(_)
        | PcmHashError::Decode(_)
        | PcmHashError::Md5Mismatch
        | PcmHashError::SampleCountMismatch { .. }
        | PcmHashError::DecoderPanic => RefusalReason::UnreadableSource,
    };
    Refusal::new(reason, e)
}

/// The source could not be read.
fn unreadable(e: impl std::fmt::Display) -> Refusal {
    Refusal::new(RefusalReason::UnreadableSource, e)
}

/// ContentHash of a file this encoder wrote: failing to read it is the
/// machine's doing.
fn staged_hash(path: &Path) -> Result<ContentHash, EnvironmentError> {
    compute_hash(path).map_err(|e| EnvironmentError::StagedFile(format!("{}: {e}", path.display())))
}

fn staged_len(path: &Path) -> Result<u64, EnvironmentError> {
    fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| EnvironmentError::StagedFile(format!("{}: {e}", path.display())))
}

/// The files of one encode, removed on drop (also on a panic or an early
/// return). A successful encode renames the FLAC to its final name first, so
/// the drop finds nothing of it.
struct Staged {
    flac: PathBuf,
    rebuilt: PathBuf,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.rebuilt);
        let _ = fs::remove_file(&self.flac);
    }
}

/// ADR-037 decision 5, every check on the final file.
fn accept(
    tool: &FlacTool,
    src: &Path,
    source: &Source,
    staged: &Staged,
) -> Result<(), EncodeError> {
    check_stream_info(&source.info, &staged.flac)?;
    check_pcm_hash(&source.pcm_hash, &staged.flac)?;
    check_rebuild(tool, &source.content_hash, staged)?;
    check_tag_facts(&source.tag_facts, &staged.flac)?;
    tags::check_pictures(src, &staged.flac)?;
    Ok(())
}

/// 5.1: COMM and STREAMINFO agree on channels, rate, frames and depth.
fn check_stream_info(aiff: &PcmInfo, flac: &Path) -> Result<(), EncodeError> {
    let mismatch = |detail: String| Refusal::new(RefusalReason::StreamInfoMismatch, detail);
    let got = pcm_info(flac).map_err(|e| staged_pcm_error(flac, e, mismatch))?;
    let same = got.channels == aiff.channels
        && got.sample_rate == aiff.sample_rate
        && got.frames == aiff.frames
        && got.bits_per_sample == aiff.bits_per_sample;
    if same {
        return Ok(());
    }
    let show = |i: &PcmInfo| {
        format!(
            "{} ch, {} Hz, {} frames, {} bits",
            i.channels, i.sample_rate, i.frames, i.bits_per_sample
        )
    };
    Err(mismatch(format!("AIFF {}, FLAC {}", show(aiff), show(&got))).into())
}

/// A `PcmHash` failure on a file this encoder wrote. I/O says nothing about the
/// file (`StagedFile`: the machine's); parse and decode errors do (`refusal`).
fn staged_pcm_error(
    flac: &Path,
    e: PcmHashError,
    refusal: impl FnOnce(String) -> Refusal,
) -> EncodeError {
    match e {
        PcmHashError::Container(AudioHashError::Io(io)) => {
            EnvironmentError::StagedFile(format!("{}: {io}", flac.display())).into()
        }
        other => refusal(format!("FLAC: {other}")).into(),
    }
}

/// 5.2: PcmHash of the FLAC, decoded by claxon, equals the AIFF's.
fn check_pcm_hash(want: &PcmHash, flac: &Path) -> Result<(), EncodeError> {
    match pcm_hash(flac) {
        Ok(got) if got == *want => Ok(()),
        Ok(got) => Err(Refusal::new(
            RefusalReason::PcmMismatch,
            format!("AIFF {}, FLAC {}", want.as_str(), got.as_str()),
        )
        .into()),
        Err(PcmHashError::Unsupported(why)) => Err(Refusal::new(
            RefusalReason::UnsupportedFormat,
            format!("the FLAC is outside PcmHash's scope: {why}"),
        )
        .into()),
        Err(e) => Err(staged_pcm_error(flac, e, |detail| {
            Refusal::new(RefusalReason::PcmMismatch, detail)
        })),
    }
}

/// 5.3: `flac -d --keep-foreign-metadata` rebuilds the AIFF byte for byte.
fn check_rebuild(tool: &FlacTool, want: &ContentHash, staged: &Staged) -> Result<(), EncodeError> {
    tool.decode(&staged.flac, &staged.rebuilt)?;
    let got = staged_hash(&staged.rebuilt)?;
    if got == *want {
        return Ok(());
    }
    Err(Refusal::new(
        RefusalReason::RebuildMismatch,
        format!("AIFF {want}, rebuilt {got}"),
    )
    .into())
}

/// 5.4: the tag facts of the FLAC are those of the AIFF, as sets of facts.
fn check_tag_facts(want: &[(MusicValue, FactSource)], flac: &Path) -> Result<(), EncodeError> {
    let mismatch = |detail: String| Refusal::new(RefusalReason::TagMismatch, detail);
    // `tag_facts` flattens its errors to text; read the tag once here so that
    // an I/O failure on the staged FLAC is told from a parse failure.
    tags::check_staged_readable(flac)?;
    let mut got = tag_facts(flac, &comparison_source())
        .map_err(|e| mismatch(format!("reading the FLAC's tags: {e}")))?;
    // A Vorbis vendor string reads back as an EncoderSoftware fact. It names
    // what wrote the FLAC (libFLAC, or nothing once lofty rewrote the block),
    // not what the AIFF's tags state. One the AIFF does state would be written
    // as the vendor and still has to match. (With lofty 0.18 none can: it reads
    // ID3 `TSSE` as EncoderSettings, which is no tag fact.)
    got.retain(|g| !matches!(g.0, MusicValue::EncoderSoftware(_)) || want.contains(g));
    let mut unmatched: Vec<&(MusicValue, FactSource)> = got.iter().collect();
    for fact in want {
        match unmatched.iter().position(|g| **g == *fact) {
            Some(i) => {
                unmatched.swap_remove(i);
            }
            None => return Err(mismatch(format!("not in the FLAC: {:?}", fact.0)).into()),
        }
    }
    match unmatched.first() {
        Some(extra) => Err(mismatch(format!("not in the AIFF: {:?}", extra.0)).into()),
        None => Ok(()),
    }
}
