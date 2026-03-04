//! Seed BDD test data into a metadata directory.
//!
//! Usage: mdma-bdd-seed <metadata-dir>

use mdma_bdd::harness::{seed_facts, SeedTrack};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <metadata-dir>", args[0]);
        std::process::exit(1);
    }

    let metadata_dir = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(metadata_dir).expect("failed to create metadata dir");

    let tracks = vec![
        SeedTrack {
            artist: "Aphex Twin".into(),
            title: "Windowlicker".into(),
            bpm: Some(137),
            genre: Some("IDM".into()),
            duration: Some(390),
            year: Some(1999),
            hash: None,
        },
        SeedTrack {
            artist: "Autechre".into(),
            title: "Gantz Graf".into(),
            bpm: Some(140),
            genre: Some("IDM".into()),
            duration: Some(281),
            year: Some(2002),
            hash: None,
        },
        SeedTrack {
            artist: "Boards of Canada".into(),
            title: "Roygbiv".into(),
            bpm: Some(92),
            genre: Some("Ambient".into()),
            duration: Some(140),
            year: Some(1998),
            hash: None,
        },
        SeedTrack {
            artist: "Squarepusher".into(),
            title: "Beep Street".into(),
            bpm: Some(160),
            genre: Some("Drill and Bass".into()),
            duration: Some(321),
            year: Some(1997),
            hash: None,
        },
        SeedTrack {
            artist: "Aphex Twin".into(),
            title: "Xtal".into(),
            bpm: Some(125),
            genre: Some("Ambient Techno".into()),
            duration: Some(290),
            year: Some(1992),
            hash: None,
        },
    ];

    seed_facts(metadata_dir, &tracks);
    println!(
        "Seeded {} tracks into {}",
        tracks.len(),
        metadata_dir.display()
    );
}
