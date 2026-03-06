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

/// Walk `info.params.EnumFormat` entries to find the highest `rate` value.
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

    let max = enum_formats
        .iter()
        .filter_map(|entry| entry.get("rate").and_then(|r| r.as_u64()))
        .max()
        .map(|r| r as u32);

    max.unwrap_or(DEFAULT_RATE)
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
                  { "mediaType": "audio", "rate": 44100 },
                  { "mediaType": "audio", "rate": 96000 }
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
                  { "mediaType": "audio", "rate": 48000 }
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
            usb.max_sample_rate, 96_000,
            "should pick the highest rate from EnumFormat"
        );

        let builtin = sinks.iter().find(|s| s.id == 7).unwrap();
        assert_eq!(builtin.max_sample_rate, 48_000);
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
