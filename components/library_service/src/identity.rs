//! Identity-hash facts (ADR-010 AudioHash v1, ADR-012 PcmHash v1).
//!
//! [`identity_facts_for_blob`] is the single place that turns a blob into identity
//! facts. Ingest and the explicit backfill both call it. A hash that cannot be
//! computed yields no fact (the error is logged and reported back), so callers
//! carry on. Each hash call is panic-guarded, so a decoder panic becomes an error
//! entry rather than killing the calling thread (ingest runs on the IPC thread).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use audio_hash::{AudioHashError, PcmHashError};
use music_facts::{FactSource, MusicValue};

/// Which identity hashes to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityOpts {
    /// AudioHash v1 (all supported containers).
    pub audio_hash: bool,
    /// PcmHash v1 (full decode; lossless FLAC/WAV/AIFF only).
    pub pcm_hash: bool,
}

impl IdentityOpts {
    /// Ingest computes AudioHash only (cheap: no decode). PcmHash is left to the backfill.
    pub const INGEST: Self = Self {
        audio_hash: true,
        pcm_hash: false,
    };

    /// The hashes a track is still missing, given the facts it already carries.
    /// AudioHash is always wanted; PcmHash only when `include_pcm`. `None` when
    /// the track has everything requested.
    pub fn missing<'a>(
        include_pcm: bool,
        existing: impl IntoIterator<Item = &'a MusicValue>,
    ) -> Option<Self> {
        let (mut has_audio, mut has_pcm) = (false, false);
        for v in existing {
            match v {
                MusicValue::AudioHash(_) => has_audio = true,
                MusicValue::PcmHash(_) => has_pcm = true,
                _ => {}
            }
        }
        let missing = Self {
            audio_hash: !has_audio,
            pcm_hash: include_pcm && !has_pcm,
        };
        (missing.audio_hash || missing.pcm_hash).then_some(missing)
    }
}

/// Facts computed for a blob, plus the errors for the hashes that could not be.
#[derive(Debug, Default)]
pub struct IdentityFacts {
    pub facts: Vec<(MusicValue, FactSource)>,
    /// One message per requested hash that failed. Expected "unsupported"
    /// outcomes (e.g. PcmHash of an MP3, AudioHash of AIFF) are not errors and
    /// are not listed.
    pub errors: Vec<String>,
}

/// Result of one guarded hash computation.
#[derive(Debug, PartialEq)]
enum Outcome<T> {
    Hash(T),
    /// Not applicable to this file; expected, so neither a fact nor an error.
    Unsupported(String),
    Failed(String),
}

/// Run `f`, turning a panic into `Outcome::Failed`.
fn guarded<T>(f: impl FnOnce() -> Outcome<T>) -> Outcome<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Outcome::Failed("hasher panicked".into()))
}

fn audio_outcome(path: &Path) -> Outcome<music_primitives::AudioHash> {
    guarded(|| match audio_hash::audio_hash(path) {
        Ok(h) => Outcome::Hash(h),
        Err(AudioHashError::UnsupportedContainer) => {
            Outcome::Unsupported("unsupported container".into())
        }
        Err(e) => Outcome::Failed(e.to_string()),
    })
}

fn pcm_outcome(path: &Path) -> Outcome<music_primitives::PcmHash> {
    guarded(|| match audio_hash::pcm_hash(path) {
        Ok(h) => Outcome::Hash(h),
        Err(PcmHashError::Unsupported(why)) => Outcome::Unsupported(why.to_string()),
        Err(PcmHashError::Container(AudioHashError::UnsupportedContainer)) => {
            Outcome::Unsupported("unsupported container".into())
        }
        Err(e) => Outcome::Failed(e.to_string()),
    })
}

/// Compute the requested identity facts for the audio file at `path`.
pub fn identity_facts_for_blob(
    path: &Path,
    opts: IdentityOpts,
    source: &FactSource,
) -> IdentityFacts {
    let mut out = IdentityFacts::default();

    if opts.audio_hash {
        match audio_outcome(path) {
            Outcome::Hash(h) => out.facts.push((MusicValue::AudioHash(h), source.clone())),
            Outcome::Unsupported(why) => {
                tracing::debug!(path = %path.display(), why, "AudioHash unsupported; no fact");
            }
            Outcome::Failed(e) => {
                tracing::warn!(path = %path.display(), error = %e, "AudioHash failed; no fact");
                out.errors.push(format!("AudioHash: {e}"));
            }
        }
    }

    if opts.pcm_hash {
        match pcm_outcome(path) {
            Outcome::Hash(h) => out.facts.push((MusicValue::PcmHash(h), source.clone())),
            Outcome::Unsupported(why) => {
                tracing::debug!(path = %path.display(), why, "PcmHash unsupported; no fact");
            }
            Outcome::Failed(e) => {
                tracing::warn!(path = %path.display(), error = %e, "PcmHash failed; no fact");
                out.errors.push(format!("PcmHash: {e}"));
            }
        }
    }

    out
}

/// Ask the kernel to drop the cached pages of `path`, so a long backfill does not
/// evict what playback has cached. Best effort: failures are ignored.
///
/// `DONTNEED` is per file, not per reader: if the track being hashed is also playing,
/// its cached pages are dropped too. That costs playback a re-read from NVMe, which
/// is not audible.
#[cfg(target_os = "linux")]
pub fn drop_page_cache(path: &Path) {
    use std::os::fd::AsRawFd;
    if let Ok(f) = std::fs::File::open(path) {
        // SAFETY: valid open fd; offset/len 0 means the whole file.
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn drop_page_cache(_path: &Path) {}

/// Lower the calling thread's CPU and IO priority so the backfill stays out of
/// playback's way. Each step that fails is logged and skipped; never fatal.
///
/// The idle IO class only takes effect under the BFQ (or CFQ) scheduler. NVMe devices
/// default to `none`, where it does nothing; there the protection comes from the
/// backfill's duty-cycle sleep and from `drop_page_cache`, not from `ioprio_set`.
#[cfg(target_os = "linux")]
pub fn lower_current_thread_priority() {
    // SAFETY: plain syscalls with integer arguments.
    let tid = unsafe { libc::gettid() };

    // CPU: nice 19 for this thread only (Linux setpriority takes a TID).
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 19) } != 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "setpriority(19) failed");
    }

    // IO: idle class for the same TID.
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            tid as libc::c_long,
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        )
    };
    if rc != 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "ioprio_set(idle) failed");
    }
}

#[cfg(not(target_os = "linux"))]
pub fn lower_current_thread_priority() {
    tracing::debug!("thread priority lowering is only implemented on Linux");
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_facts::FactOrigin;
    use pretty_assertions::assert_eq;

    pub const S16_L8: &[u8] = include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16_l8.flac");
    pub const S16_L8_TAGS: &[u8] =
        include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16_l8_tags.flac");
    const S16_WAV: &[u8] = include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16.wav");
    const S16_AIFF: &[u8] = include_bytes!("../../audio_hash/testdata/pcm/fixtures/s16.aiff");

    fn source() -> FactSource {
        FactSource::new("test", "0", FactOrigin::Unknown)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn audio_of(f: &IdentityFacts) -> Vec<&MusicValue> {
        f.facts
            .iter()
            .map(|(v, _)| v)
            .filter(|v| matches!(v, MusicValue::AudioHash(_)))
            .collect()
    }

    #[test]
    fn ingest_opts_give_audio_hash_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.flac", S16_L8);
        let f = identity_facts_for_blob(&p, IdentityOpts::INGEST, &source());
        assert!(f.errors.is_empty());
        assert_eq!(f.facts.len(), 1);
        assert!(matches!(f.facts[0].0, MusicValue::AudioHash(_)));
    }

    #[test]
    fn retagged_files_get_equal_audio_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.flac", S16_L8);
        let b = write(dir.path(), "b.flac", S16_L8_TAGS);
        let fa = identity_facts_for_blob(&a, IdentityOpts::INGEST, &source());
        let fb = identity_facts_for_blob(&b, IdentityOpts::INGEST, &source());
        assert_eq!(audio_of(&fa), audio_of(&fb));
        assert_eq!(audio_of(&fa).len(), 1);
    }

    #[test]
    fn pcm_hash_added_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.flac", S16_L8);
        let opts = IdentityOpts {
            audio_hash: true,
            pcm_hash: true,
        };
        let f = identity_facts_for_blob(&p, opts, &source());
        assert!(f.errors.is_empty());
        assert_eq!(f.facts.len(), 2);
        assert!(matches!(f.facts[1].0, MusicValue::PcmHash(_)));
    }

    #[test]
    fn pcm_hash_is_shared_across_containers() {
        let dir = tempfile::tempdir().unwrap();
        let flac = write(dir.path(), "a.flac", S16_L8);
        let wav = write(dir.path(), "a.wav", S16_WAV);
        let opts = IdentityOpts {
            audio_hash: false,
            pcm_hash: true,
        };
        let a = identity_facts_for_blob(&flac, opts, &source());
        let b = identity_facts_for_blob(&wav, opts, &source());
        assert_eq!(a.facts[0].0, b.facts[0].0);
    }

    #[test]
    fn garbage_gives_no_fact_and_no_error_when_container_is_unrecognised() {
        // Unrecognised bytes are "unsupported", not a failure (M3).
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.flac", b"this is not audio at all");
        let opts = IdentityOpts {
            audio_hash: true,
            pcm_hash: true,
        };
        let f = identity_facts_for_blob(&p, opts, &source());
        assert!(f.facts.is_empty());
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    #[test]
    fn aiff_has_no_audio_hash_and_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.aiff", S16_AIFF);
        let f = identity_facts_for_blob(&p, IdentityOpts::INGEST, &source());
        assert!(f.facts.is_empty());
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    #[test]
    fn corrupt_flac_gives_no_fact_and_an_error() {
        // Valid FLAC magic, truncated before any audio: recognised but malformed.
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.flac", &S16_L8[..20]);
        let f = identity_facts_for_blob(&p, IdentityOpts::INGEST, &source());
        assert!(f.facts.is_empty());
        assert_eq!(f.errors.len(), 1);
    }

    #[test]
    fn missing_file_gives_no_fact_and_an_error() {
        let f = identity_facts_for_blob(
            Path::new("/nonexistent/x.flac"),
            IdentityOpts::INGEST,
            &source(),
        );
        assert!(f.facts.is_empty());
        assert_eq!(f.errors.len(), 1);
    }

    #[test]
    fn a_panicking_hasher_becomes_a_failure() {
        let out: Outcome<u8> = guarded(|| panic!("decoder blew up"));
        assert_eq!(out, Outcome::Failed("hasher panicked".into()));
        assert_eq!(guarded(|| Outcome::Hash(7u8)), Outcome::Hash(7));
    }

    #[test]
    fn missing_reports_only_what_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.flac", S16_L8);
        let opts = IdentityOpts {
            audio_hash: true,
            pcm_hash: true,
        };
        let facts = identity_facts_for_blob(&path, opts, &source()).facts;
        let (audio, pcm) = (&facts[0].0, &facts[1].0);

        assert_eq!(IdentityOpts::missing(false, []), Some(IdentityOpts::INGEST));
        assert_eq!(IdentityOpts::missing(false, [audio]), None);
        assert_eq!(
            IdentityOpts::missing(true, [audio]),
            Some(IdentityOpts {
                audio_hash: false,
                pcm_hash: true
            })
        );
        assert_eq!(IdentityOpts::missing(true, [audio, pcm]), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lowering_priority_sets_nice_19_on_the_thread() {
        let nice = std::thread::spawn(|| {
            lower_current_thread_priority();
            // SAFETY: plain syscalls.
            unsafe { libc::getpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t) }
        })
        .join()
        .unwrap();
        assert_eq!(nice, 19);
    }

    #[test]
    fn dropping_page_cache_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.flac", S16_L8);
        drop_page_cache(&p);
        drop_page_cache(Path::new("/nonexistent"));
    }

    #[test]
    #[ignore = "timing measurement; run with --ignored --nocapture"]
    fn audio_hash_cost_per_track_is_measured() {
        // Prints the cost of AudioHash on a ~84 MB FLAC-sized payload.
        let mut big = S16_L8.to_vec();
        let frames_start = big.len() / 2;
        let tail = big[frames_start..].to_vec();
        while big.len() < 84 * 1024 * 1024 {
            big.extend_from_slice(&tail);
        }
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "big.flac", &big);
        let t = std::time::Instant::now();
        let f = identity_facts_for_blob(&p, IdentityOpts::INGEST, &source());
        println!("AudioHash of {} MB: {:?}", big.len() >> 20, t.elapsed());
        assert_eq!(f.facts.len(), 1);
    }
}
