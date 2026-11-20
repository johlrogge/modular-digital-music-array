//! Synthetic audio-file bytes for tests (ADR-037): IFF/AIFF primitives and a
//! builder for whole AIFF / AIFF-C files with ID3 tags. Everything is built in
//! memory; no binary fixtures, no real audio, no dependency on the code under test.

pub mod testkit;

/// ID3v2 syncsafe encoding of `n` (28 bits).
pub fn syncsafe(n: u32) -> [u8; 4] {
    debug_assert!(n < 1 << 28, "{n} does not fit in 28 syncsafe bits");
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
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
