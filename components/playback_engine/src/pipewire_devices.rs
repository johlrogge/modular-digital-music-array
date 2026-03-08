use crate::PlaybackError;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioSink {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub max_sample_rate: u32,
}

/// Internal structure for deserialising pw-dump entries.
#[derive(Debug, Deserialize)]
struct PwEntry {
    id: u32,
    info: Option<PwInfo>,
}

#[derive(Debug, Deserialize)]
struct PwInfo {
    props: Option<Value>,
    params: Option<Value>,
}

/// Parse pw-dump JSON text into a list of AudioSink values.
/// Exposed for testing so tests do not need PipeWire running.
pub fn parse_sinks(json: &str) -> Result<Vec<AudioSink>, PlaybackError> {
    let entries: Vec<PwEntry> = serde_json::from_str(json)
        .map_err(|e| PlaybackError::AudioDevice(format!("pw-dump parse error: {e}")))?;

    let mut sinks = Vec::new();

    for entry in entries {
        let Some(info) = entry.info else { continue };
        let Some(props) = info.props else { continue };

        let media_class = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if media_class != "Audio/Sink" {
            continue;
        }

        let name = props
            .get("node.name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let description = props
            .get("node.description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let max_sample_rate = extract_max_rate(info.params.as_ref());

        sinks.push(AudioSink {
            id: entry.id,
            name,
            description,
            max_sample_rate,
        });
    }

    Ok(sinks)
}

/// Resolve a `rate` JSON value to a u64 sample rate.
///
/// PipeWire pw-dump can represent rate as either a bare integer or an object
/// of the form `{"default": N, "min": N, "max": N}`. This helper handles both.
fn rate_from_value(v: &Value) -> Option<u64> {
    // Bare integer (simplified pw-dump or older PipeWire)
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    // Object form: { "default": N, "min": N, "max": N }
    if let Some(obj) = v.as_object() {
        return obj
            .get("max")
            .or_else(|| obj.get("default"))
            .and_then(|n| n.as_u64());
    }
    None
}

/// Returns true when a format entry describes a PCM (raw) audio stream.
/// DSD and other non-PCM subtypes are excluded from sample-rate comparisons
/// so they cannot inflate the reported maximum PCM rate.
fn is_pcm_entry(entry: &Value) -> bool {
    entry.get("mediaSubtype").and_then(|v| v.as_str()) == Some("raw")
}

/// Walk `info.params.EnumFormat` entries to find the highest `rate` value.
/// PCM entries (mediaSubtype == "raw") are preferred; if none carry a
/// `mediaSubtype` field the function falls back to all entries so that
/// older pw-dump output without that field still works.
/// Returns 48000 when no rate information is available.
fn extract_max_rate(params: Option<&Value>) -> u32 {
    const DEFAULT_RATE: u32 = 48_000;

    let params = match params {
        Some(p) => p,
        None => return DEFAULT_RATE,
    };

    let enum_formats = match params.get("EnumFormat").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return DEFAULT_RATE,
    };

    let rate_from_entry = |entry: &Value| entry.get("rate").and_then(rate_from_value);

    // Prefer PCM-only entries; fall back to all if none have mediaSubtype.
    let pcm_max = enum_formats
        .iter()
        .filter(|e| is_pcm_entry(e))
        .filter_map(rate_from_entry)
        .max();

    let max = pcm_max.or_else(|| enum_formats.iter().filter_map(rate_from_entry).max());

    max.map(|r| r as u32).unwrap_or(DEFAULT_RATE)
}

/// Enumerate audio sinks by running `pw-dump` and parsing its JSON output.
pub fn list_sinks() -> Result<Vec<AudioSink>, PlaybackError> {
    let output = std::process::Command::new("pw-dump")
        .output()
        .map_err(|e| PlaybackError::AudioDevice(format!("failed to run pw-dump: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PlaybackError::AudioDevice(format!(
            "pw-dump exited with status {}: {stderr}",
            output.status
        )));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    parse_sinks(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_sinks_fixture() -> &'static str {
        r#"[
          {
            "id": 42,
            "type": "PipeWire:Interface:Node",
            "info": {
              "props": {
                "media.class": "Audio/Sink",
                "node.name": "alsa_output.usb-1",
                "node.description": "USB Audio Output"
              },
              "params": {
                "EnumFormat": [
                  { "mediaType": "audio", "mediaSubtype": "raw", "rate": { "default": 48000, "min": 44100, "max": 384000 } },
                  { "mediaType": "audio", "mediaSubtype": "raw", "rate": { "default": 48000, "min": 44100, "max": 96000 } }
                ]
              }
            }
          },
          {
            "id": 99,
            "type": "PipeWire:Interface:Node",
            "info": {
              "props": {
                "media.class": "Audio/Source",
                "node.name": "alsa_input.usb-1",
                "node.description": "USB Audio Input"
              },
              "params": {}
            }
          },
          {
            "id": 7,
            "type": "PipeWire:Interface:Node",
            "info": {
              "props": {
                "media.class": "Audio/Sink",
                "node.name": "alsa_output.pci-0",
                "node.description": "Built-in Audio"
              },
              "params": {
                "EnumFormat": [
                  { "mediaType": "audio", "mediaSubtype": "raw", "rate": { "default": 48000, "min": 44100, "max": 48000 } },
                  { "mediaType": "audio", "mediaSubtype": "raw", "rate": { "default": 48000, "min": 44100, "max": 48000 } }
                ]
              }
            }
          }
        ]"#
    }

    fn dsd_sink_fixture() -> &'static str {
        r#"[
          {
            "id": 55,
            "type": "PipeWire:Interface:Node",
            "info": {
              "props": {
                "media.class": "Audio/Sink",
                "node.name": "alsa_output.dsd-dac",
                "node.description": "DSD DAC"
              },
              "params": {
                "EnumFormat": [
                  { "mediaType": "audio", "mediaSubtype": "raw", "rate": { "default": 48000, "min": 44100, "max": 384000 } },
                  { "mediaType": "audio", "mediaSubtype": "dsd", "rate": { "default": 48000, "min": 44100, "max": 1536000 } }
                ]
              }
            }
          }
        ]"#
    }

    fn no_subtype_fixture() -> &'static str {
        r#"[
          {
            "id": 88,
            "type": "PipeWire:Interface:Node",
            "info": {
              "props": {
                "media.class": "Audio/Sink",
                "node.name": "alsa_output.legacy",
                "node.description": "Legacy Sink"
              },
              "params": {
                "EnumFormat": [
                  { "mediaType": "audio", "rate": { "default": 48000, "min": 44100, "max": 192000 } },
                  { "mediaType": "audio", "rate": { "default": 48000, "min": 44100, "max": 96000 } }
                ]
              }
            }
          }
        ]"#
    }

    #[test]
    fn parses_fixture_and_filters_non_sink_nodes() {
        let sinks = parse_sinks(two_sinks_fixture()).expect("parse should succeed");
        assert_eq!(sinks.len(), 2, "should return exactly 2 sinks");

        let ids: Vec<u32> = sinks.iter().map(|s| s.id).collect();
        assert!(ids.contains(&42));
        assert!(ids.contains(&7));
        assert!(!ids.contains(&99), "source node must be filtered out");

        let usb = sinks.iter().find(|s| s.id == 42).unwrap();
        assert_eq!(usb.name, "alsa_output.usb-1");
        assert_eq!(usb.description, "USB Audio Output");
    }

    #[test]
    fn handles_empty_pw_dump_output() {
        let sinks = parse_sinks("[]").expect("empty array should parse cleanly");
        assert!(sinks.is_empty());
    }

    #[test]
    fn extracts_max_sample_rate_from_enum_format() {
        let sinks = parse_sinks(two_sinks_fixture()).expect("parse should succeed");
        let usb = sinks.iter().find(|s| s.id == 42).unwrap();
        assert_eq!(
            usb.max_sample_rate, 384_000,
            "should pick the highest rate from EnumFormat"
        );

        let builtin = sinks.iter().find(|s| s.id == 7).unwrap();
        assert_eq!(builtin.max_sample_rate, 48_000);
    }

    #[test]
    fn rate_from_value_handles_bare_integer() {
        let v = serde_json::json!(96000u64);
        assert_eq!(rate_from_value(&v), Some(96000));
    }

    #[test]
    fn rate_from_value_uses_default_when_max_absent() {
        let v = serde_json::json!({ "default": 48000 });
        assert_eq!(rate_from_value(&v), Some(48000));
    }

    #[test]
    fn dsd_entries_are_excluded_from_max_sample_rate() {
        let sinks = parse_sinks(dsd_sink_fixture()).expect("parse should succeed");
        assert_eq!(sinks.len(), 1);
        assert_eq!(
            sinks[0].max_sample_rate, 384_000,
            "DSD entry with rate 1536000 must not inflate max_sample_rate"
        );
    }

    #[test]
    fn entries_without_media_subtype_fall_back_to_all_entries() {
        let sinks = parse_sinks(no_subtype_fixture()).expect("parse should succeed");
        assert_eq!(sinks.len(), 1);
        assert_eq!(
            sinks[0].max_sample_rate, 192_000,
            "when no entry has mediaSubtype, should fall back to max across all entries"
        );
    }

    #[test]
    fn defaults_max_rate_to_48000_when_params_absent() {
        let json = r#"[
          {
            "id": 1,
            "info": {
              "props": {
                "media.class": "Audio/Sink",
                "node.name": "dummy",
                "node.description": "Dummy Sink"
              }
            }
          }
        ]"#;
        let sinks = parse_sinks(json).expect("parse should succeed");
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].max_sample_rate, 48_000);
    }
}
