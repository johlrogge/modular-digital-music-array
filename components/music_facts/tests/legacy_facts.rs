//! Legacy fact lines (copied from a real ACID facts log) must still load.

use music_facts::{FactSource, MusicValue};
use stainless_facts::Fact;

type MusicFact = Fact<music_facts::ContentHash, MusicValue, FactSource>;

const FIXTURE: &str = include_str!("fixtures/legacy_facts.jsonl");

#[test]
fn legacy_fixture_loads_with_zero_failures() {
    let failures: Vec<String> = FIXTURE
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<MusicFact>(line)
                .err()
                .map(|e| format!("{e}: {line}"))
        })
        .collect();
    assert!(failures.is_empty(), "failed lines: {failures:#?}");
    assert_eq!(FIXTURE.lines().count(), 7);
}

/// Decodes a fixture line as a whole `Fact` and returns its value in the
/// current wire format (the value is the second element of the fact tuple).
fn decoded_value(line_no: usize) -> serde_json::Value {
    let line = FIXTURE.lines().nth(line_no).unwrap();
    let fact: MusicFact = serde_json::from_str(line).unwrap();
    serde_json::to_value(&fact).unwrap()[1].clone()
}

#[test]
fn legacy_fixture_lines_decode_to_expected_values() {
    use serde_json::json;
    // Line 1: HasAlbumArt true -> Present
    assert_eq!(decoded_value(0), json!({"t":"HasAlbumArt","v":"Present"}));
    // Line 4: Skipped -> TrackStopped(OnSkip)
    assert_eq!(decoded_value(3), json!({"t":"TrackStopped","v":"OnSkip"}));
    // Line 6: Played -> TrackStopped(OnCompletion)
    assert_eq!(
        decoded_value(5),
        json!({"t":"TrackStopped","v":"OnCompletion"})
    );
}

/// Parses every line of a full facts log. Run manually:
/// `LEGACY_FACTS_LOG=/path/to/facts.jsonl cargo test -- --ignored full_log`
#[test]
#[ignore = "needs LEGACY_FACTS_LOG=<path to a real facts.jsonl>"]
fn full_log_loads_with_zero_failures() {
    let path = std::env::var("LEGACY_FACTS_LOG").expect("set LEGACY_FACTS_LOG");
    let text = std::fs::read_to_string(path).unwrap();
    let failed = text
        .lines()
        .filter(|l| serde_json::from_str::<MusicFact>(l).is_err())
        .count();
    assert_eq!(failed, 0);
}
