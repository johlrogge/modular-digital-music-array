use std::path::Path;

use crate::PlaybackError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AudioOutputConfig {
    /// Selected device name, or None for PipeWire auto-connect
    pub device_name: Option<String>,
    /// Output sample rate in Hz
    pub sample_rate: u32,
}

impl Default for AudioOutputConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 192_000, // Current default, matches iFi HD USB Audio
        }
    }
}

/// Load audio output config from a JSON file. Returns default if file doesn't exist.
pub fn load_audio_config(path: &Path) -> Result<AudioOutputConfig, PlaybackError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let config = serde_json::from_str(&contents)
                .map_err(|e| PlaybackError::AudioDevice(format!("Config parse error: {e}")))?;
            Ok(config)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AudioOutputConfig::default()),
        Err(e) => Err(PlaybackError::Io(e)),
    }
}

/// Save audio output config to a JSON file (atomic write via temp file + rename).
pub fn save_audio_config(path: &Path, config: &AudioOutputConfig) -> Result<(), PlaybackError> {
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| PlaybackError::AudioDevice(format!("Config serialize error: {e}")))?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_device_and_192k_rate() {
        let config = AudioOutputConfig::default();
        assert_eq!(config.device_name, None);
        assert_eq!(config.sample_rate, 192_000);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let path = tmp_dir.path().join("nonexistent.json");
        let config = load_audio_config(&path).expect("load should succeed");
        assert_eq!(config, AudioOutputConfig::default());
    }

    #[test]
    fn round_trip_save_and_load() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let path = tmp_dir.path().join("audio_config.json");

        let original = AudioOutputConfig {
            device_name: Some("iFi HD USB Audio".to_string()),
            sample_rate: 96_000,
        };

        save_audio_config(&path, &original).expect("save should succeed");
        let loaded = load_audio_config(&path).expect("load should succeed");

        assert_eq!(original, loaded);
    }

    #[test]
    fn round_trip_with_no_device_name() {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let path = tmp_dir.path().join("audio_config.json");

        let original = AudioOutputConfig::default();
        save_audio_config(&path, &original).expect("save should succeed");
        let loaded = load_audio_config(&path).expect("load should succeed");

        assert_eq!(original, loaded);
    }
}
