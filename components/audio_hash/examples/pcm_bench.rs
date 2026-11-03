//! Benchmark PcmHash (and AudioHash v1) over a directory tree.
//!
//! ```text
//! pcm_bench [--no-v1] [--limit N] <dir>
//! ```
//!
//! Per file, three timed passes over the same bytes:
//! - `pcm_ms`: the full public `pcm_hash` (decode + SHA-256 + MD5 check);
//! - `decode_ms`: claxon decode only (FLAC; empty for WAV/AIFF), to separate
//!   decoder cost from hashing without instrumenting the crate;
//! - `sha_ms`: SHA-256 over the raw file, the floor for any hash of the bytes.
//!
//! PcmHash minus decode is roughly hashing plus MD5. `v1_ms` is `audio_hash`.
//! Run it twice (cold, then warm cache) to separate I/O from CPU.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use audio_hash::{audio_hash, pcm_hash, AudioHashError, PcmHashError};
use sha2::{Digest, Sha256};

const EXTENSIONS: &[&str] = &["flac", "wav", "aiff", "aif", "aifc"];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn error_key(e: &PcmHashError) -> String {
    match e {
        PcmHashError::Container(AudioHashError::Malformed { container, reason }) => {
            format!("Container/Malformed {container:?}: {reason}")
        }
        PcmHashError::Container(AudioHashError::Io(e)) => format!("Container/Io: {:?}", e.kind()),
        PcmHashError::Container(other) => format!("Container: {other}"),
        PcmHashError::Unsupported(r) => format!("Unsupported: {r}"),
        PcmHashError::Decode(r) => format!("Decode: {r}"),
        PcmHashError::Md5Mismatch => "Md5Mismatch".to_owned(),
        PcmHashError::SampleCountMismatch { .. } => "SampleCountMismatch".to_owned(),
        PcmHashError::DecoderPanic => "DecoderPanic".to_owned(),
    }
}

/// Audio duration in seconds from the container header, if cheaply known.
fn audio_seconds(path: &Path) -> Option<f64> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    f.seek(SeekFrom::Start(0)).ok()?;
    match &head[..4] {
        b"fLaC" => {
            let r = claxon::FlacReader::new(f).ok()?;
            let si = r.streaminfo();
            Some(si.samples? as f64 / f64::from(si.sample_rate))
        }
        b"RIFF" => wav_seconds(&mut f),
        b"FORM" => aiff_seconds(&mut f),
        _ => None,
    }
}

/// Walk IFF chunks after the 12-byte header: (id, payload start, size).
fn chunks(f: &mut File, big_endian: bool) -> Vec<([u8; 4], u64, u64)> {
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut out = Vec::new();
    let mut pos = 12u64;
    while pos + 8 <= len {
        let mut h = [0u8; 8];
        if f.seek(SeekFrom::Start(pos)).is_err() || f.read_exact(&mut h).is_err() {
            break;
        }
        let raw = [h[4], h[5], h[6], h[7]];
        let size = u64::from(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        });
        out.push(([h[0], h[1], h[2], h[3]], pos + 8, size));
        pos += 8 + size + (size & 1);
    }
    out
}

fn wav_seconds(f: &mut File) -> Option<f64> {
    let cs = chunks(f, false);
    let (_, fmt_at, _) = cs.iter().find(|c| &c.0 == b"fmt ")?;
    let (_, _, data_len) = cs.iter().find(|c| &c.0 == b"data")?;
    let mut fmt = [0u8; 16];
    f.seek(SeekFrom::Start(*fmt_at)).ok()?;
    f.read_exact(&mut fmt).ok()?;
    let rate = u32::from_le_bytes(fmt[4..8].try_into().ok()?);
    let align = u16::from_le_bytes(fmt[12..14].try_into().ok()?);
    (rate > 0 && align > 0).then(|| *data_len as f64 / f64::from(align) / f64::from(rate))
}

fn aiff_seconds(f: &mut File) -> Option<f64> {
    let cs = chunks(f, true);
    let (_, at, _) = cs.iter().find(|c| &c.0 == b"COMM")?;
    let mut comm = [0u8; 18];
    f.seek(SeekFrom::Start(*at)).ok()?;
    f.read_exact(&mut comm).ok()?;
    let frames = u32::from_be_bytes(comm[2..6].try_into().ok()?);
    // 80-bit extended rate: sign+15-bit exponent, 64-bit mantissa.
    let exp = i32::from(u16::from_be_bytes([comm[8], comm[9]]) & 0x7FFF) - 16383;
    let mant = u64::from_be_bytes(comm[10..18].try_into().ok()?);
    let rate = mant as f64 * 2f64.powi(exp - 63);
    (rate > 0.0).then(|| f64::from(frames) / rate)
}

/// claxon decode only; `None` for non-FLAC or on any error.
fn decode_only(path: &Path) -> Option<()> {
    let mut r = claxon::FlacReader::open(path).ok()?;
    let mut blocks = r.blocks();
    let mut buf = Vec::new();
    while let Some(b) = blocks.read_next_or_eof(buf).ok()? {
        buf = b.into_buffer();
    }
    Some(())
}

fn sha_file(path: &Path) -> std::io::Result<()> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    std::hint::black_box(h.finalize());
    Ok(())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[derive(Default)]
struct Totals {
    files: u64,
    bytes: u64,
    audio_s: f64,
    pcm_ms: f64,
    v1_ms: f64,
    decode_ms: f64,
    sha_ms: f64,
    errors: BTreeMap<String, u64>,
}

fn per_gb(ms: f64, bytes: u64) -> f64 {
    ms / 1000.0 / (bytes as f64 / 1e9)
}

fn main() -> ExitCode {
    let mut no_v1 = false;
    let mut limit = usize::MAX;
    let mut dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-v1" => no_v1 = true,
            "--limit" => match args.next().and_then(|n| n.parse().ok()) {
                Some(n) => limit = n,
                None => return usage(),
            },
            _ if dir.is_none() && !a.starts_with("--") => dir = Some(PathBuf::from(a)),
            _ => return usage(),
        }
    }
    let Some(dir) = dir else { return usage() };

    let mut files = Vec::new();
    if let Err(e) = collect(&dir, &mut files) {
        eprintln!("cannot read {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    files.sort();
    files.truncate(limit);

    println!(
        "#path\tbytes\taudio_s\tpcm_ms\trealtime_x\t{}decode_ms\tsha_ms\tresult",
        if no_v1 { "" } else { "v1_ms\t" }
    );
    let mut t = Totals::default();
    for path in &files {
        let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let audio_s = audio_seconds(path);

        let start = Instant::now();
        let res = pcm_hash(path);
        let pcm_ms = ms(start);

        let v1 = (!no_v1).then(|| {
            let start = Instant::now();
            let _ = audio_hash(path);
            ms(start)
        });
        let start = Instant::now();
        let decoded = decode_only(path);
        let decode_ms = decoded.map(|()| ms(start));
        let start = Instant::now();
        let _ = sha_file(path);
        let sha_ms = ms(start);

        let result = match &res {
            Ok(_) => "ok".to_owned(),
            Err(e) => error_key(e),
        };
        let opt = |v: Option<f64>| v.map_or("-".to_owned(), |v| format!("{v:.3}"));
        let realtime = audio_s.map(|s| s / (pcm_ms / 1000.0));
        println!(
            "{}\t{bytes}\t{}\t{pcm_ms:.3}\t{}\t{}{}\t{sha_ms:.3}\t{result}",
            path.display(),
            opt(audio_s),
            opt(realtime),
            v1.map_or(String::new(), |v| format!("{v:.3}\t")),
            opt(decode_ms),
        );

        t.files += 1;
        t.bytes += bytes;
        t.audio_s += audio_s.unwrap_or(0.0);
        t.pcm_ms += pcm_ms;
        t.v1_ms += v1.unwrap_or(0.0);
        t.decode_ms += decode_ms.unwrap_or(0.0);
        t.sha_ms += sha_ms;
        if result != "ok" {
            *t.errors.entry(result).or_default() += 1;
        }
    }

    println!("# files: {}", t.files);
    println!("# bytes: {} ({:.3} GB)", t.bytes, t.bytes as f64 / 1e9);
    println!("# audio hours: {:.3}", t.audio_s / 3600.0);
    if t.bytes > 0 {
        let report = |name: &str, ms: f64| {
            let gb = per_gb(ms, t.bytes);
            println!(
                "# {name}: total {:.1} s, {gb:.1} s/GB, 30 GB ~ {:.1} min",
                ms / 1000.0,
                gb * 30.0 / 60.0
            );
        };
        report("pcm", t.pcm_ms);
        if !no_v1 {
            report("v1", t.v1_ms);
        }
        report("decode-only (flac)", t.decode_ms);
        report("sha256 raw", t.sha_ms);
    }
    println!("# errors by variant+reason:");
    if t.errors.is_empty() {
        println!("#   (none)");
    }
    for (k, n) in &t.errors {
        println!("#   {n}\t{k}");
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: pcm_bench [--no-v1] [--limit N] <dir>");
    ExitCode::from(2)
}
