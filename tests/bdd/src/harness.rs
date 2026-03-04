//! Test harness — boots real services in-process and creates clients for the World.

use crate::playback_simulator::{start_playback_simulator, PlaybackState};
use library_service::LibraryService;
use mdma_client::{LibraryBackend, PlaybackBackend};
use music_facts::{
    Artist, Bpm, ContentHash, DurationSeconds, FactOrigin, FactSource, MusicValue, Title, Year,
};
use stainless_facts::{Fact, FactStreamWriter, Operation};
use std::sync::{Arc, Mutex};

/// A seeded track definition for BDD Background steps.
pub struct SeedTrack {
    pub artist: String,
    pub title: String,
    pub bpm: Option<u16>,
    pub genre: Option<String>,
    pub duration: Option<u32>,
    pub year: Option<u32>,
    /// Explicit hex hash (e.g. "a1b2c3d4"). Padded to 64 hex chars with trailing zeros.
    /// When None, an index-based hash is auto-generated.
    pub hash: Option<String>,
}

/// Everything the World needs from the harness.
pub struct TestEnv {
    pub library: LibraryBackend,
    pub playback: PlaybackBackend,
    pub playback_state: Arc<Mutex<PlaybackState>>,
    pub library_addr: String,
    pub playback_addr: String,
    pub _temp_dir: tempfile::TempDir,
    pub _library_thread: std::thread::JoinHandle<()>,
    pub _playback_thread: std::thread::JoinHandle<()>,
}

/// Seed a facts.jsonl file with the given tracks.
fn seed_facts(metadata_dir: &std::path::Path, tracks: &[SeedTrack]) {
    let facts_path = metadata_dir.join("facts.jsonl");
    let mut writer = FactStreamWriter::open(&facts_path).expect("failed to open fact stream");

    let source = FactSource::new("bdd-seed", "0.0.0", FactOrigin::Unknown);
    let now = chrono::Utc::now();

    for (i, track) in tracks.iter().enumerate() {
        // Use explicit hash if provided (padded to 64 hex chars), else auto-generate from index
        let hash = match &track.hash {
            Some(h) => {
                let padded = format!("{:0<64}", h);
                ContentHash(format!("sha256:{}", padded))
            }
            None => ContentHash(format!("sha256:{:064x}", i + 1)),
        };

        let mut facts: Vec<Fact<ContentHash, MusicValue, FactSource>> = vec![
            Fact::new(
                hash.clone(),
                MusicValue::Title(Title::new(&track.title)),
                now,
                source.clone(),
                Operation::Assert,
            ),
            Fact::new(
                hash.clone(),
                MusicValue::Artist(Artist::new(&track.artist)),
                now,
                source.clone(),
                Operation::Assert,
            ),
        ];

        if let Some(bpm_val) = track.bpm {
            if let Ok(bpm) = Bpm::from_f32(bpm_val as f32) {
                facts.push(Fact::new(
                    hash.clone(),
                    MusicValue::Bpm(bpm),
                    now,
                    source.clone(),
                    Operation::Assert,
                ));
            }
        }

        if let Some(ref genre) = track.genre {
            facts.push(Fact::new(
                hash.clone(),
                MusicValue::MainGenre(genre.clone()),
                now,
                source.clone(),
                Operation::Assert,
            ));
        }

        if let Some(dur) = track.duration {
            facts.push(Fact::new(
                hash.clone(),
                MusicValue::DurationSeconds(DurationSeconds(dur)),
                now,
                source.clone(),
                Operation::Assert,
            ));
        }

        if let Some(year) = track.year {
            facts.push(Fact::new(
                hash.clone(),
                MusicValue::Year(Year(year)),
                now,
                source.clone(),
                Operation::Assert,
            ));
        }

        writer.write_batch(&facts).expect("failed to write facts");
    }
}

/// Boot a complete test environment with the given seed tracks.
pub fn boot_test_env(tracks: &[SeedTrack]) -> TestEnv {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");

    let music_dir = temp_dir.path().join("music");
    let metadata_dir = temp_dir.path().join("metadata");
    std::fs::create_dir_all(music_dir.join("inbox")).unwrap();
    std::fs::create_dir_all(music_dir.join("blobs")).unwrap();
    std::fs::create_dir_all(&metadata_dir).unwrap();

    // Seed facts
    seed_facts(&metadata_dir, tracks);

    // Create library service
    let library_service = LibraryService::new(
        music_dir.clone(),
        metadata_dir,
        "ipc:///tmp/mdma-bdd-acid-nonexistent.sock",
    )
    .expect("failed to create service");
    let library_service = Arc::new(library_service);

    // Choose unique IPC addresses (avoid collisions between parallel tests)
    let id = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let library_addr = format!(
        "ipc://{}/mdma-bdd-library-{}-{}.sock",
        temp_dir.path().display(),
        id,
        ts
    );
    let playback_addr = format!(
        "ipc://{}/mdma-bdd-playback-{}-{}.sock",
        temp_dir.path().display(),
        id,
        ts
    );

    // Start library IPC server in background thread
    let lib_svc = Arc::clone(&library_service);
    let lib_addr = library_addr.clone();
    let library_thread = std::thread::spawn(move || {
        let _ = library_service::service::run_ipc_server(lib_svc, &lib_addr, None);
    });

    // Give the library server a moment to bind
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Start playback simulator
    let (playback_state, playback_thread) = start_playback_simulator(&playback_addr);

    // Create clients
    let library =
        LibraryBackend::connect_direct(&library_addr).expect("failed to connect library client");
    let playback =
        PlaybackBackend::connect_direct(&playback_addr).expect("failed to connect playback client");

    TestEnv {
        library,
        playback,
        playback_state,
        library_addr,
        playback_addr,
        _temp_dir: temp_dir,
        _library_thread: library_thread,
        _playback_thread: playback_thread,
    }
}
