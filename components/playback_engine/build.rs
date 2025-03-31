use anyhow::{Context, Result};
use hound::WavWriter;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::process::Command;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

/// Estimates the size of a FLAC file based on duration
fn expected_size(duration_secs: f32) -> u64 {
    let samples = (duration_secs * SAMPLE_RATE as f32 * CHANNELS as f32) as u64;
    samples * 2 // Approximate FLAC compression of sine waves
}

/// Check if a file needs to be generated
fn should_generate(path: &Path, duration: f32) -> bool {
    match path.metadata() {
        Ok(metadata) => {
            let expected = expected_size(duration);
            let actual = metadata.len();
            // Regenerate if file size is significantly different
            (actual < expected / 2) || (actual > expected * 2)
        }
        Err(_) => true, // File doesn't exist
    }
}

/// Generate a test signal with multiple frequencies
fn generate_test_signal(duration_secs: f32) -> Vec<f32> {
    let num_samples = (duration_secs * SAMPLE_RATE as f32) as usize * CHANNELS as usize;
    let mut samples = Vec::with_capacity(num_samples);

    // Generate a mix of frequencies for testing (A4, A5, A6)
    let frequencies = [440.0, 880.0, 1760.0];

    for i in 0..num_samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        let sample = frequencies
            .iter()
            .enumerate()
            .map(|(idx, &freq)| {
                let amplitude = 0.25 / (idx + 1) as f32;
                amplitude * (2.0 * PI * freq * t).sin()
            })
            .sum::<f32>();

        samples.push(sample);
    }

    samples
}
use std::fs;

// Add this function to build.rs
fn generate_test_pattern_wav(path: PathBuf, pattern_type: &str) -> Result<()> {
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = WavWriter::create(&path, spec)
        .with_context(|| format!("Failed to create WAV file: {}", path.display()))?;

    // Generate 0.5 seconds of audio with specific patterns (48000 samples per sec * 0.5 * 2 channels)
    let sample_count = (SAMPLE_RATE as usize * CHANNELS as usize) / 2;

    match pattern_type {
        "alternating" => {
            // Alternating max, zero, min pattern
            for i in 0..sample_count {
                let sample = match i % 3 {
                    0 => 0.9,  // Near max (+1.0)
                    1 => 0.0,  // Zero
                    _ => -0.9, // Near min (-1.0)
                };
                writer.write_sample(sample)?;
            }
        }
        "ascending" => {
            // Ascending ramp from -0.9 to 0.9
            for i in 0..sample_count {
                let sample = -0.9 + (1.8 * i as f32 / sample_count as f32);
                writer.write_sample(sample)?;
            }
        }
        "silence" => {
            // All zeros
            for _ in 0..sample_count {
                writer.write_sample(0.0)?;
            }
        }
        "impulses" => {
            // Periodic impulses (good for checking segmentation)
            for i in 0..sample_count {
                let sample = if i % 100 == 0 { 0.9 } else { 0.0 };
                writer.write_sample(sample)?;
            }
        }
        _ => return Err(anyhow::anyhow!("Unknown pattern type")),
    }

    writer.finalize()?;
    Ok(())
}

/// Write samples to a WAV file
fn write_test_wav(path: PathBuf, duration: f32) -> Result<()> {
    let samples = generate_test_signal(duration);

    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = WavWriter::create(&path, spec)
        .with_context(|| format!("Failed to create WAV file: {}", path.display()))?;

    for sample in samples {
        writer
            .write_sample(sample)
            .with_context(|| format!("Failed to write sample to WAV file: {}", path.display()))?;
    }
    writer.finalize()?;

    Ok(())
}

/// Convert WAV to FLAC using ffmpeg
fn convert_to_flac(wav_path: &PathBuf, flac_path: &PathBuf) -> Result<()> {
    // Check if ffmpeg is available
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        println!("cargo:warning=ffmpeg not found, test files will remain as WAV");
        // Just rename the WAV file to FLAC in this case
        std::fs::rename(wav_path, flac_path)?;
        return Ok(());
    }

    let status = Command::new("ffmpeg")
        .arg("-y") // Overwrite output files
        .arg("-i")
        .arg(wav_path)
        .arg("-c:a")
        .arg("flac")
        .arg("-compression_level")
        .arg("12")
        .arg(flac_path)
        .status()
        .with_context(|| "Failed to run ffmpeg")?;

    if !status.success() {
        anyhow::bail!("ffmpeg failed to convert WAV to FLAC");
    }

    // Remove the temporary WAV file
    std::fs::remove_file(wav_path)?;

    //println!("cargo:warning=Generated test file: {}", flac_path.display());
    //println!(
    //    "cargo:warning=File size: {} bytes",
    //    flac_path.metadata()?.len()
    //);

    Ok(())
}

fn main() -> Result<()> {
    // Only rerun if this build script changes
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from("benches/test_data");

    // Create test files of different lengths
    let test_files = vec![
        ("short.flac", 5.0),   // 5 seconds
        ("medium.flac", 30.0), // 30 seconds
        ("long.flac", 180.0),  // 3 minutes
    ];

    for (name, duration) in test_files {
        let flac_path = out_dir.join(name);

        if should_generate(&flac_path, duration) {
            //println!("cargo:warning=Generating {}", name);
            let wav_path = out_dir.join(format!("{}.wav", name.strip_suffix(".flac").unwrap()));
            write_test_wav(wav_path.clone(), duration)?;
            convert_to_flac(&wav_path, &flac_path)?;
        } else {
            //println!("cargo:warning=Skipping {} (already exists)", name);
        }
    }

    // Create test pattern files
    let test_patterns = vec![
        ("alternating.flac", "alternating"),
        ("ascending.flac", "ascending"),
        ("silence.flac", "silence"),
        ("impulses.flac", "impulses"),
    ];

    for (name, pattern) in test_patterns {
        let flac_path = out_dir.join(name);

        if should_generate(&flac_path, 0.5) {
            // 0.5 seconds duration
            //println!("cargo:warning=Generating pattern file {}", name);
            let wav_path = out_dir.join(format!("{}.wav", name.strip_suffix(".flac").unwrap()));
            generate_test_pattern_wav(wav_path.clone(), pattern)?;
            convert_to_flac(&wav_path, &flac_path)?;
        } else {
            println!("cargo:warning=Skipping {} (already exists)", name);
        }
    }

    Ok(())
}
