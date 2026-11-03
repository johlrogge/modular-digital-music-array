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
