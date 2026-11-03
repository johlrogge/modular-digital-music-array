//! Synthetic byte builders for tests. No external tools, no real media files.
//!
//! NOTE: these builders are free to evolve for ordinary tests, but the golden
//! hashes (G-1) and the spec-preimage tests in `lib.rs` deliberately do NOT use
//! them: those inputs are spelled out as literal bytes there. Do not make the
//! golden tests depend on this module.

use std::io::Cursor;
use std::ops::Range;

use music_primitives::AudioHash;

use crate::{audio_hash_reader, AudioHashError, Container};

pub fn hash_of(bytes: &[u8]) -> AudioHash {
    audio_hash_reader(Cursor::new(bytes)).expect("hashable fixture")
}

type LayoutResult = Result<Vec<Range<u64>>, AudioHashError>;

/// Run a format module's `layout` over an in-memory buffer.
pub fn lay_with(
    layout: fn(&mut Cursor<Vec<u8>>, u64) -> LayoutResult,
    bytes: &[u8],
) -> LayoutResult {
    layout(&mut Cursor::new(bytes.to_vec()), bytes.len() as u64)
}

/// True when `r` is `Malformed` for exactly `container`.
pub fn is_malformed<T>(r: Result<T, AudioHashError>, container: Container) -> bool {
    matches!(r, Err(AudioHashError::Malformed { container: c, .. }) if c == container)
}

/// ID3v1 extended ("TAG+", 227 bytes) block.
pub fn id3v1_extended() -> Vec<u8> {
    let mut v = b"TAG+".to_vec();
    v.extend(vec![b'e'; 223]);
    v
}

pub fn syncsafe(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

/// Head (or appended, when `footer`) ID3v2 tag with a `payload_len` body.
pub fn id3v2(payload_len: usize, footer: bool) -> Vec<u8> {
    let flags = if footer { 0x10 } else { 0 };
    let mut v = b"ID3".to_vec();
    v.extend([4, 0, flags]);
    v.extend(syncsafe(payload_len as u32));
    v.extend(vec![0x41; payload_len]);
    if footer {
        v.extend(b"3DI");
        v.extend([4, 0, flags]);
        v.extend(syncsafe(payload_len as u32));
    }
    v
}

pub fn id3v1() -> Vec<u8> {
    let mut v = b"TAG".to_vec();
    v.extend(vec![b'x'; 125]);
    v
}

/// APEv2 tag: optional header, `items_len` item bytes, footer.
pub fn ape(items_len: usize, header: bool) -> Vec<u8> {
    let size = (items_len + 32) as u32;
    let flags: u32 = if header { 0x8000_0000 } else { 0 };
    let part = |is_header: bool| {
        let mut p = b"APETAGEX".to_vec();
        p.extend(2000u32.to_le_bytes());
        p.extend(size.to_le_bytes());
        p.extend(1u32.to_le_bytes());
        let f = if is_header {
            flags | 0x2000_0000
        } else {
            flags
        };
        p.extend(f.to_le_bytes());
        p.extend([0u8; 8]);
        p
    };
    let mut v = Vec::new();
    if header {
        v.extend(part(true));
    }
    v.extend(vec![b'i'; items_len]);
    v.extend(part(false));
    v
}

/// Lyrics3v2 block of `len` body bytes (>= 11) plus the 15-byte trailer.
pub fn lyrics3v2(len: usize) -> Vec<u8> {
    let len = len.max(11);
    let mut v = b"LYRICSBEGIN".to_vec();
    v.extend(vec![b'l'; len.saturating_sub(11)]);
    v.extend(format!("{len:06}").bytes());
    v.extend(b"LYRICS200");
    v
}

pub fn streaminfo() -> (u8, Vec<u8>) {
    (0, vec![0x11; 34])
}
pub fn padding(n: usize) -> (u8, Vec<u8>) {
    (1, vec![0; n])
}
pub fn vorbis(n: usize) -> (u8, Vec<u8>) {
    (4, vec![b'v'; n])
}
pub fn picture(n: usize) -> (u8, Vec<u8>) {
    (6, vec![0x50; n])
}

/// `n` fake FLAC frames, each starting `FF F8`.
pub fn flac_frames(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend([0xFF, 0xF8]);
        v.extend(vec![(i as u8).wrapping_add(1); 30]);
    }
    v
}

pub fn flac(blocks: &[(u8, Vec<u8>)], frames: &[u8]) -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    for (i, (ty, body)) in blocks.iter().enumerate() {
        let last = if i + 1 == blocks.len() { 0x80 } else { 0 };
        v.push(last | ty);
        v.extend(&(body.len() as u32).to_be_bytes()[1..]);
        v.extend(body);
    }
    v.extend(frames);
    v
}

/// FLAC frame-header CRC-8 (poly 0x07, init 0).
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut crc, &b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// FLAC frame CRC-16 (poly 0x8005, init 0).
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, &b| {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// One subframe of a hand-encoded 16-bit FLAC frame.
pub enum Sub<'a> {
    Verbatim(&'a [i32]),
    Constant(i32),
}

/// A valid 16-bit, 44100 Hz FLAC frame with independent channels (1 or 2
/// subframes), `block` samples (1..=256) per channel, fixed blocking strategy
/// and a frame number below 128. Header CRC-8 and frame CRC-16 included.
pub fn flac_frame16(frame_no: u8, block: usize, subs: &[Sub]) -> Vec<u8> {
    assert!((1..=256).contains(&block) && frame_no < 128 && (1..=2).contains(&subs.len()));
    let mut f = vec![
        0xFF,
        0xF8,                                   // sync, fixed blocking
        0x69,                                   // block size from 8-bit field, 44100 Hz
        (((subs.len() - 1) as u8) << 4) | 0x08, // independent channels, 16 bits
        frame_no,
        (block - 1) as u8,
    ];
    f.push(crc8(&f));
    for s in subs {
        match s {
            Sub::Constant(v) => {
                f.push(0x00);
                f.extend((*v as i16).to_be_bytes());
            }
            Sub::Verbatim(vals) => {
                assert_eq!(vals.len(), block);
                f.push(0x02);
                f.extend(be_samples(vals, 2));
            }
        }
    }
    f.extend(crc16(&f).to_be_bytes());
    f
}

/// STREAMINFO payload (34 bytes). Frame sizes are left unknown (0).
pub fn flac_streaminfo(
    rate: u32,
    channels: u8,
    bits: u8,
    total_samples: u64,
    md5: [u8; 16],
) -> Vec<u8> {
    let mut v = 16u16.to_be_bytes().to_vec(); // min block size
    v.extend(256u16.to_be_bytes()); // max block size
    v.extend([0u8; 6]); // min/max frame size unknown
    let packed: u64 = (u64::from(rate) << 44)
        | (u64::from(channels - 1) << 41)
        | (u64::from(bits - 1) << 36)
        | total_samples;
    v.extend(packed.to_be_bytes());
    v.extend(md5);
    v
}

/// `n` fake MP3 frames, each starting `FF FB`.
pub fn mp3_frames(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend([0xFF, 0xFB, 0x90, 0x04]);
        v.extend(vec![(i as u8) | 0x01; 96]);
    }
    v
}

pub fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = chunk_sized(id, payload.len() as u32, payload);
    if payload.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// Chunk with an explicit size field and no padding.
pub fn chunk_sized(id: &[u8; 4], size: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(size.to_le_bytes());
    v.extend(payload);
    v
}

pub fn fmt_chunk(tag: u8) -> Vec<u8> {
    chunk(b"fmt ", &[tag; 16])
}

pub fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut v = b"RIFF".to_vec();
    v.extend(((body.len() + 4) as u32).to_le_bytes());
    v.extend(b"WAVE");
    v.extend(body);
    v
}

/// `fmt ` payload (16 bytes, WAVEFORMAT) with real field values.
pub fn wav_fmt_payload(tag: u16, channels: u16, rate: u32, block_align: u16, bits: u16) -> Vec<u8> {
    let mut v = tag.to_le_bytes().to_vec();
    v.extend(channels.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * u32::from(block_align)).to_le_bytes());
    v.extend(block_align.to_le_bytes());
    v.extend(bits.to_le_bytes());
    v
}

pub fn wav_fmt(tag: u16, channels: u16, rate: u32, block_align: u16, bits: u16) -> Vec<u8> {
    chunk(
        b"fmt ",
        &wav_fmt_payload(tag, channels, rate, block_align, bits),
    )
}

/// WAVE_FORMAT_EXTENSIBLE `fmt ` chunk (40-byte payload).
pub fn wav_fmt_ext(
    channels: u16,
    rate: u32,
    block_align: u16,
    bits: u16,
    valid_bits: u16,
    mask: u32,
    subformat: u32,
) -> Vec<u8> {
    let mut p = wav_fmt_payload(0xFFFE, channels, rate, block_align, bits);
    p.extend(22u16.to_le_bytes()); // cbSize
    p.extend(valid_bits.to_le_bytes());
    p.extend(mask.to_le_bytes());
    p.extend(subformat.to_le_bytes());
    p.extend([
        0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
    ]);
    chunk(b"fmt ", &p)
}

/// Low `bytes` bytes of each sample, little-endian.
pub fn le_samples(samples: &[i32], bytes: usize) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|s| s.to_le_bytes().into_iter().take(bytes))
        .collect()
}

/// Low `bytes` bytes of each sample, big-endian.
pub fn be_samples(samples: &[i32], bytes: usize) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|s| s.to_be_bytes().into_iter().skip(4 - bytes))
        .collect()
}

/// IFF chunk with a big-endian size, padded to even length.
pub fn be_chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((payload.len() as u32).to_be_bytes());
    v.extend(payload);
    if payload.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// 80-bit IEEE extended encoding of a positive integer.
pub fn ext80(v: u32) -> [u8; 10] {
    let v = u64::from(v);
    let lz = v.leading_zeros();
    let exp = (16383 + 63 - lz) as u16;
    let mant = v << lz;
    let mut b = [0u8; 10];
    b[..2].copy_from_slice(&exp.to_be_bytes());
    b[2..].copy_from_slice(&mant.to_be_bytes());
    b
}

/// COMM chunk with explicit 10-byte rate and extra tail bytes (AIFF-C fields).
pub fn comm_with(
    channels: u16,
    frames: u32,
    sample_size: u16,
    rate: [u8; 10],
    tail: &[u8],
) -> Vec<u8> {
    let mut p = channels.to_be_bytes().to_vec();
    p.extend(frames.to_be_bytes());
    p.extend(sample_size.to_be_bytes());
    p.extend(rate);
    p.extend(tail);
    be_chunk(b"COMM", &p)
}

/// Plain AIFF COMM chunk (18 bytes).
pub fn comm(channels: u16, frames: u32, sample_size: u16, rate: u32) -> Vec<u8> {
    comm_with(channels, frames, sample_size, ext80(rate), &[])
}

/// AIFF-C COMM chunk with the given compression type and an empty name.
pub fn comm_c(channels: u16, frames: u32, sample_size: u16, rate: u32, ctype: &[u8; 4]) -> Vec<u8> {
    let mut tail = ctype.to_vec();
    tail.extend([0, 0]); // empty pstring, padded
    comm_with(channels, frames, sample_size, ext80(rate), &tail)
}

/// SSND chunk: `offset` filler bytes precede the sample data.
pub fn ssnd(offset: u32, data: &[u8]) -> Vec<u8> {
    let mut p = offset.to_be_bytes().to_vec();
    p.extend(0u32.to_be_bytes()); // blockSize
    p.extend(vec![0x77; offset as usize]);
    p.extend(data);
    be_chunk(b"SSND", &p)
}

/// `FORM` container of the given form type (`AIFF` or `AIFC`).
pub fn aiff(form: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut v = b"FORM".to_vec();
    v.extend(((body.len() + 4) as u32).to_be_bytes());
    v.extend(form);
    v.extend(body);
    v
}

pub fn mp4_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(payload);
    v
}

pub fn mp4_box_large(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = 1u32.to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(((payload.len() + 16) as u64).to_be_bytes());
    v.extend(payload);
    v
}

/// Box with size == 0 (runs to EOF).
pub fn mp4_box_to_eof(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = 0u32.to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(payload);
    v
}

pub fn ftyp() -> Vec<u8> {
    mp4_box(b"ftyp", b"M4A \0\0\0\0M4A isom")
}

pub fn mp4(boxes: &[Vec<u8>]) -> Vec<u8> {
    boxes.concat()
}
