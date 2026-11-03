//! PcmHash v1 (ADR-012): an exact hash of decoded lossless PCM samples,
//! independent of container, bit depth padding and tags.
//!
//! `sha256( b"mdma-pcmhash\0v1\0" || 0x01 || u8(channels) || u32_le(rate)
//!          || for each frame, for each channel: i32_le(s << (32 - stored_bits)) )`

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::num::NonZeroU32;
use std::path::Path;

use md5::Md5;
use music_primitives::PcmHash;
use sha2::{Digest, Sha256};

use crate::{sniff, AudioHashError, Container, BUF_SIZE};

mod aiff;
mod flac;
mod raw;
mod wav;

const DOMAIN: &[u8] = b"mdma-pcmhash\0v1\0";
/// Sample format byte: signed integer PCM. Other values are reserved.
const FORMAT_SIGNED_INT: u8 = 0x01;

#[derive(Debug, thiserror::Error)]
pub enum PcmHashError {
    #[error("container: {0}")]
    Container(#[from] AudioHashError),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("decode: {0}")]
    Decode(&'static str),
    #[error("decoded samples do not match the STREAMINFO MD5")]
    Md5Mismatch,
    #[error("sample count mismatch: declared {declared}, decoded {decoded}")]
    SampleCountMismatch { declared: u64, decoded: u64 },
    #[error("decoder panicked")]
    DecoderPanic,
}

impl From<io::Error> for PcmHashError {
    fn from(e: io::Error) -> Self {
        PcmHashError::Container(AudioHashError::Io(e))
    }
}

impl PcmHashError {
    fn malformed(container: Container, reason: &'static str) -> Self {
        PcmHashError::Container(container.malformed(reason))
    }

    fn no_audio() -> Self {
        PcmHashError::Container(AudioHashError::NoAudio)
    }
}

/// Hash the decoded samples of the file at `path`.
pub fn pcm_hash(path: &Path) -> Result<PcmHash, PcmHashError> {
    pcm_hash_reader(File::open(path)?)
}

/// Hash the decoded samples of a seekable byte source.
pub fn pcm_hash_reader<R: Read + Seek>(mut r: R) -> Result<PcmHash, PcmHashError> {
    let file_len = r.seek(SeekFrom::End(0))?;
    let container = sniff::sniff(&mut r, file_len)?;
    match container {
        Container::Flac => flac::hash(&mut r, file_len),
        Container::Mp3 => Err(PcmHashError::Unsupported("mp3: lossy")),
        Container::Mp4 => Err(PcmHashError::Unsupported("mp4: not supported")),
        Container::Wav => {
            let layout = wav::layout(&mut r, file_len)?;
            raw::hash(&mut r, &layout)
        }
        Container::Aiff => {
            let layout = aiff::layout(&mut r, file_len)?;
            raw::hash(&mut r, &layout)
        }
    }
}

/// Mono or stereo; ADR-012 scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    pub fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }
}

impl TryFrom<u32> for Channels {
    type Error = PcmHashError;

    fn try_from(n: u32) -> Result<Self, PcmHashError> {
        match n {
            0 => Err(PcmHashError::Decode("zero channels")),
            1 => Ok(Channels::Mono),
            2 => Ok(Channels::Stereo),
            _ => Err(PcmHashError::Unsupported("more than 2 channels")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SampleRate(NonZeroU32);

impl SampleRate {
    pub fn new(hz: u32) -> Option<Self> {
        NonZeroU32::new(hz).map(SampleRate)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// Bits per stored sample, 1..=32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoredBits(u8);

impl StoredBits {
    pub fn new(bits: u8) -> Option<Self> {
        (1..=32).contains(&bits).then_some(StoredBits(bits))
    }

    /// Left shift that justifies a stored sample to 32 bits.
    pub fn shift(self) -> u32 {
        32 - u32::from(self.0)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    /// Bytes per sample in the MD5 packing: `ceil(bits / 8)`.
    pub fn bytes(self) -> usize {
        usize::from(self.0).div_ceil(8)
    }
}

/// Result of [`PcmHasher::finish`].
pub(crate) struct PcmDigest {
    pub hash: PcmHash,
    pub frames: u64,
    /// Present when the MD5 side-channel was enabled: the MD5 of the
    /// right-justified, little-endian, `ceil(bits / 8)`-byte packed samples,
    /// i.e. FLAC's STREAMINFO MD5 form.
    pub md5: Option<[u8; 16]>,
}

/// Feeds samples into the PcmHash v1 preimage.
///
/// Samples are passed right-justified (plain signed values that fit
/// `bits`); the hasher left-justifies them to 32 bits.
pub(crate) struct PcmHasher {
    sha: Sha256,
    buf: Vec<u8>,
    channels: Channels,
    bits: StoredBits,
    samples: u64,
    /// MD5 side-channel: hasher plus its packing buffer (flushed with `buf`).
    md5: Option<(Md5, Vec<u8>)>,
}

impl PcmHasher {
    /// Starts a hash and writes the preimage header.
    pub fn new(channels: Channels, rate: SampleRate, bits: StoredBits) -> Self {
        let mut sha = Sha256::new();
        sha.update(DOMAIN);
        sha.update([FORMAT_SIGNED_INT, channels.count() as u8]);
        sha.update(rate.get().to_le_bytes());
        PcmHasher {
            sha,
            buf: Vec::with_capacity(BUF_SIZE + 4),
            channels,
            bits,
            samples: 0,
            md5: None,
        }
    }

    /// Also MD5 the packed samples (streaming; nothing is retained).
    pub fn with_md5(mut self) -> Self {
        self.md5 = Some((Md5::new(), Vec::with_capacity(BUF_SIZE + 4)));
        self
    }

    /// Interleaved samples, channels in file order.
    pub fn write_interleaved(&mut self, samples: &[i32]) {
        for &s in samples {
            self.push(s);
        }
    }

    /// One slice per channel, all of equal length.
    pub fn write_planar(&mut self, planes: &[&[i32]]) -> Result<(), PcmHashError> {
        if planes.len() != self.channels.count() {
            return Err(PcmHashError::Decode("wrong channel count"));
        }
        let len = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != len) {
            return Err(PcmHashError::Decode("unequal channel lengths"));
        }
        for i in 0..len {
            for p in planes {
                self.push(p[i]);
            }
        }
        Ok(())
    }

    fn push(&mut self, s: i32) {
        self.buf
            .extend_from_slice(&(s << self.bits.shift()).to_le_bytes());
        if let Some((_, packed)) = &mut self.md5 {
            packed.extend_from_slice(&s.to_le_bytes()[..self.bits.bytes()]);
        }
        self.samples += 1;
        if self.buf.len() >= BUF_SIZE {
            self.flush();
        }
    }

    fn flush(&mut self) {
        self.sha.update(&self.buf);
        self.buf.clear();
        if let Some((md5, packed)) = &mut self.md5 {
            md5.update(packed.as_slice());
            packed.clear();
        }
    }

    pub fn finish(mut self) -> Result<PcmDigest, PcmHashError> {
        self.flush();
        let ch = self.channels.count() as u64;
        if !self.samples.is_multiple_of(ch) {
            return Err(PcmHashError::Decode("partial trailing frame"));
        }
        Ok(PcmDigest {
            hash: PcmHash::from_sha256_v1(self.sha.finalize().into()),
            frames: self.samples / ch,
            md5: self.md5.map(|(m, _)| m.finalize().into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    /// Hand-built ADR-012 preimage from interleaved, already left-justified
    /// samples.
    fn spec_pcm_hash(rate: u32, channels: u8, left_justified: &[i32]) -> String {
        let mut h = Sha256::new();
        h.update(b"mdma-pcmhash\0v1\0");
        h.update([0x01u8, channels]);
        h.update(rate.to_le_bytes());
        for s in left_justified {
            h.update(s.to_le_bytes());
        }
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        format!("pcm-sha256-v1:{hex}")
    }

    fn lj(vals: &[i32], bits: u32) -> Vec<i32> {
        vals.iter().map(|v| v << (32 - bits)).collect()
    }

    fn pcm(bytes: &[u8]) -> Result<PcmHash, PcmHashError> {
        pcm_hash_reader(Cursor::new(bytes))
    }

    fn ok(bytes: &[u8]) -> PcmHash {
        pcm(bytes).expect("hashable fixture")
    }

    const VALS: [i32; 6] = [0, -1, 32767, -32768, 1, 256];

    fn wav16(channels: u16, rate: u32, vals: &[i32]) -> Vec<u8> {
        riff(&[
            wav_fmt(1, channels, rate, 2 * channels, 16),
            chunk(b"data", &le_samples(vals, 2)),
        ])
    }

    fn aiff16(channels: u16, rate: u32, vals: &[i32]) -> Vec<u8> {
        aiff(
            b"AIFF",
            &[
                comm(
                    channels,
                    (vals.len() / usize::from(channels)) as u32,
                    16,
                    rate,
                ),
                ssnd(0, &be_samples(vals, 2)),
            ],
        )
    }

    fn is_malformed(r: Result<PcmHash, PcmHashError>) -> bool {
        matches!(
            r,
            Err(PcmHashError::Container(AudioHashError::Malformed { .. }))
        )
    }

    fn is_unsupported(r: Result<PcmHash, PcmHashError>) -> bool {
        matches!(r, Err(PcmHashError::Unsupported(_)))
    }

    fn is_no_audio(r: Result<PcmHash, PcmHashError>) -> bool {
        matches!(r, Err(PcmHashError::Container(AudioHashError::NoAudio)))
    }

    // G-1: golden PcmHash, computed once from `spec_pcm_hash`. It freezes
    // algorithm v1 (ADR-012): changing it requires a v2 prefix and an ADR
    // update, not an edit of this string.
    const GOLDEN_PCM: &str =
        "pcm-sha256-v1:69d790cf0a46bec70c6e11124c55d2072b6eb7b9199e48de859d0945c602e2e6";

    // Golden inputs are spelled out byte by byte (not via `fixtures`).
    // 16-bit stereo 44100 Hz: (0,-1), (32767,-32768), (1,256).
    fn golden_wav() -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend(48u32.to_le_bytes());
        v.extend(b"WAVE");
        v.extend(b"fmt ");
        v.extend(16u32.to_le_bytes());
        v.extend([
            0x01, 0x00, // PCM
            0x02, 0x00, // 2 channels
            0x44, 0xAC, 0x00, 0x00, // 44100
            0x10, 0xB1, 0x02, 0x00, // byte rate 176400
            0x04, 0x00, // block align
            0x10, 0x00, // 16 bits
        ]);
        v.extend(b"data");
        v.extend(12u32.to_le_bytes());
        v.extend([
            0x00, 0x00, 0xFF, 0xFF, // (0, -1)
            0xFF, 0x7F, 0x00, 0x80, // (32767, -32768)
            0x01, 0x00, 0x00, 0x01, // (1, 256)
        ]);
        v
    }

    fn golden_aiff() -> Vec<u8> {
        let mut v = b"FORM".to_vec();
        v.extend(58u32.to_be_bytes());
        v.extend(b"AIFF");
        v.extend(b"COMM");
        v.extend(18u32.to_be_bytes());
        v.extend([
            0x00, 0x02, // 2 channels
            0x00, 0x00, 0x00, 0x03, // 3 frames
            0x00, 0x10, // 16 bits
            0x40, 0x0E, 0xAC, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 44100
        ]);
        v.extend(b"SSND");
        v.extend(20u32.to_be_bytes());
        v.extend([0u8; 8]); // offset, blockSize
        v.extend([
            0x00, 0x00, 0xFF, 0xFF, // (0, -1)
            0x7F, 0xFF, 0x80, 0x00, // (32767, -32768)
            0x00, 0x01, 0x01, 0x00, // (1, 256)
        ]);
        v
    }

    fn golden_left_justified() -> [i32; 6] {
        [0, -1 << 16, 32767 << 16, i32::MIN, 1 << 16, 256 << 16]
    }

    #[test]
    fn g1_golden_pcm_hash() {
        let spec = spec_pcm_hash(44100, 2, &golden_left_justified());
        assert_eq!(spec, GOLDEN_PCM);
        assert_eq!(ok(&golden_wav()).as_str(), GOLDEN_PCM);
        assert_eq!(ok(&golden_aiff()).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn fixtures_match_the_spelled_out_goldens() {
        assert_eq!(wav16(2, 44100, &VALS), golden_wav());
        assert_eq!(aiff16(2, 44100, &VALS), golden_aiff());
    }

    // --- types and hasher ---

    #[test]
    fn channels_accept_only_mono_and_stereo() {
        assert_eq!(Channels::try_from(1).unwrap(), Channels::Mono);
        assert_eq!(Channels::try_from(2).unwrap(), Channels::Stereo);
        assert!(matches!(
            Channels::try_from(3),
            Err(PcmHashError::Unsupported(_))
        ));
        assert!(matches!(
            Channels::try_from(0),
            Err(PcmHashError::Decode(_))
        ));
    }

    #[test]
    fn sample_rate_is_nonzero() {
        assert!(SampleRate::new(0).is_none());
        assert_eq!(SampleRate::new(48000).unwrap().get(), 48000);
    }

    #[test]
    fn stored_bits_range_shift_and_bytes() {
        assert!(StoredBits::new(0).is_none());
        assert!(StoredBits::new(33).is_none());
        for (bits, shift, bytes) in [
            (1, 31, 1),
            (8, 24, 1),
            (12, 20, 2),
            (16, 16, 2),
            (24, 8, 3),
            (32, 0, 4),
        ] {
            let b = StoredBits::new(bits).unwrap();
            assert_eq!((b.shift(), b.bytes()), (shift, bytes), "{bits}");
        }
    }

    fn hasher(ch: Channels, bits: u8) -> PcmHasher {
        PcmHasher::new(
            ch,
            SampleRate::new(44100).unwrap(),
            StoredBits::new(bits).unwrap(),
        )
    }

    #[test]
    fn hasher_matches_spec_and_ignores_stored_width() {
        let want = spec_pcm_hash(44100, 2, &golden_left_justified());
        let mut h16 = hasher(Channels::Stereo, 16);
        h16.write_interleaved(&VALS);
        let d16 = h16.finish().unwrap();
        assert_eq!(d16.hash.as_str(), want);
        assert_eq!(d16.frames, 3);
        assert!(d16.md5.is_none());

        let v24: Vec<i32> = VALS.iter().map(|v| v * 256).collect();
        let mut h24 = hasher(Channels::Stereo, 24);
        h24.write_interleaved(&v24);
        assert_eq!(h24.finish().unwrap().hash.as_str(), want);
    }

    #[test]
    fn planar_equals_interleaved_and_is_validated() {
        let mut a = hasher(Channels::Stereo, 16);
        a.write_interleaved(&VALS);
        let mut b = hasher(Channels::Stereo, 16);
        b.write_planar(&[&[0, 32767, 1], &[-1, -32768, 256]])
            .unwrap();
        assert_eq!(a.finish().unwrap().hash, b.finish().unwrap().hash);

        let mut c = hasher(Channels::Stereo, 16);
        assert!(c.write_planar(&[&[1, 2]]).is_err());
        assert!(c.write_planar(&[&[1, 2], &[1]]).is_err());
    }

    #[test]
    fn partial_frame_is_rejected_at_finish() {
        let mut h = hasher(Channels::Stereo, 16);
        h.write_interleaved(&[1, 2, 3]);
        assert!(matches!(h.finish(), Err(PcmHashError::Decode(_))));
    }

    #[test]
    fn buffer_flush_boundary_is_invisible() {
        let n = 3 * BUF_SIZE / 4 + 18; // even: whole stereo frames
        let vals: Vec<i32> = (0..n as i32).map(|i| (i * 7919) % 30000 - 15000).collect();
        let mut h = hasher(Channels::Stereo, 16);
        h.write_interleaved(&vals);
        let d = h.finish().unwrap();
        assert_eq!(d.frames, (n / 2) as u64);
        assert_eq!(d.hash.as_str(), spec_pcm_hash(44100, 2, &lj(&vals, 16)));
    }

    #[test]
    fn md5_packing_is_right_justified_little_endian() {
        // 12-bit: 2 bytes each. -1 = ff ff, 0x7ff = ff 07, -2048 = 00 f8.
        fn md5_of(bits: u8, samples: &[i32]) -> [u8; 16] {
            let mut h = hasher(Channels::Mono, bits).with_md5();
            h.write_interleaved(samples);
            h.finish().unwrap().md5.unwrap()
        }
        let md5 = |bytes: &[u8]| -> [u8; 16] { Md5::digest(bytes).into() };
        assert_eq!(
            md5_of(12, &[-1, 0x7FF, -2048]),
            md5(&[0xff, 0xff, 0xff, 0x07, 0x00, 0xf8])
        );
        // 24-bit: 3 bytes each.
        assert_eq!(
            md5_of(24, &[-1, 1, -8_388_608]),
            md5(&[0xff, 0xff, 0xff, 0x01, 0x00, 0x00, 0x00, 0x00, 0x80])
        );
        // 8-bit: 1 byte each.
        assert_eq!(md5_of(8, &[-128, 127, -1]), md5(&[0x80, 0x7f, 0xff]));
        // The side-channel does not change the hash, and is off by default.
        let mut plain = hasher(Channels::Mono, 8);
        plain.write_interleaved(&[-128, 127, -1]);
        let mut with = hasher(Channels::Mono, 8).with_md5();
        with.write_interleaved(&[-128, 127, -1]);
        assert_eq!(plain.finish().unwrap().hash, with.finish().unwrap().hash);
    }

    #[test]
    fn md5_streams_across_flush_boundaries() {
        let n = 2 * BUF_SIZE + 5;
        let vals: Vec<i32> = (0..n as i32).map(|i| (i * 7919) % 60000 - 30000).collect();
        let mut h = hasher(Channels::Mono, 16).with_md5();
        h.write_interleaved(&vals);
        let packed: Vec<u8> = vals
            .iter()
            .flat_map(|s| s.to_le_bytes()[..2].to_vec())
            .collect();
        let want: [u8; 16] = Md5::digest(&packed).into();
        assert_eq!(h.finish().unwrap().md5.unwrap(), want);
    }

    #[test]
    fn error_conversions() {
        let e: PcmHashError = io::Error::other("x").into();
        assert!(matches!(e, PcmHashError::Container(AudioHashError::Io(_))));
        assert!(!PcmHashError::Md5Mismatch.to_string().is_empty());
        assert!(!PcmHashError::DecoderPanic.to_string().is_empty());
        assert!(PcmHashError::SampleCountMismatch {
            declared: 1,
            decoded: 2
        }
        .to_string()
        .contains('2'));
    }

    // --- dispatch ---

    #[test]
    fn other_containers_are_unsupported() {
        assert!(is_unsupported(pcm(&mp3_frames(2))));
        assert!(is_unsupported(pcm(&mp4(&[
            ftyp(),
            mp4_box(b"mdat", b"abcd")
        ]))));
        assert!(matches!(
            pcm(b"hello world!"),
            Err(PcmHashError::Container(
                AudioHashError::UnsupportedContainer
            ))
        ));
    }

    #[test]
    fn path_api_works() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        std::fs::write(&p, golden_wav()).unwrap();
        assert_eq!(pcm_hash(&p).unwrap().as_str(), GOLDEN_PCM);
        assert!(matches!(
            pcm_hash(&dir.path().join("missing")),
            Err(PcmHashError::Container(AudioHashError::Io(_)))
        ));
    }

    // --- equality decisions ---

    #[test]
    fn wav_24_bit_equals_16_bit() {
        let v24: Vec<i32> = VALS.iter().map(|v| v * 256).collect();
        let w24 = riff(&[
            wav_fmt(1, 2, 44100, 6, 24),
            chunk(b"data", &le_samples(&v24, 3)),
        ]);
        assert_eq!(ok(&w24).as_str(), GOLDEN_PCM);
        // 32-bit container too
        let v32: Vec<i32> = VALS.iter().map(|v| v << 16).collect();
        let w32 = riff(&[
            wav_fmt(1, 2, 44100, 8, 32),
            chunk(b"data", &le_samples(&v32, 4)),
        ]);
        assert_eq!(ok(&w32).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn eight_bit_wav_unsigned_equals_aiff_signed() {
        let signed = [0, -128, 127, -1, 1, 100];
        let unsigned: Vec<u8> = signed.iter().map(|s| (s + 128) as u8).collect();
        let wav = riff(&[wav_fmt(1, 2, 44100, 2, 8), chunk(b"data", &unsigned)]);
        let aif = aiff(
            b"AIFF",
            &[comm(2, 3, 8, 44100), ssnd(0, &be_samples(&signed, 1))],
        );
        let want = spec_pcm_hash(44100, 2, &lj(&signed, 8));
        assert_eq!(ok(&wav).as_str(), want);
        assert_eq!(ok(&aif).as_str(), want);
    }

    #[test]
    fn aiff_odd_sample_size_uses_container_width() {
        // 12-bit samples sit left-justified in 16-bit containers.
        let aif = aiff(
            b"AIFF",
            &[comm(2, 3, 12, 44100), ssnd(0, &be_samples(&VALS, 2))],
        );
        assert_eq!(ok(&aif).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn aiff_c_variants_equal_aiff() {
        for (ctype, big) in [(b"NONE", true), (b"twos", true), (b"sowt", false)] {
            let data = if big {
                be_samples(&VALS, 2)
            } else {
                le_samples(&VALS, 2)
            };
            let aifc = aiff(b"AIFC", &[comm_c(2, 3, 16, 44100, ctype), ssnd(0, &data)]);
            assert_eq!(ok(&aifc).as_str(), GOLDEN_PCM, "{ctype:?}");
        }
    }

    #[test]
    fn extra_chunks_and_tag_placement_do_not_matter() {
        let data = chunk(b"data", &le_samples(&VALS, 2));
        let fmt = wav_fmt(1, 2, 44100, 4, 16);
        let list = chunk(b"LIST", b"INFOISFT\x05\0\0\0Lavf\0");
        let id3 = chunk(b"id3 ", &id3v2(31, false));
        let odd = chunk(b"junk", &[1, 2, 3]);
        for (i, w) in [
            riff(&[list.clone(), fmt.clone(), data.clone()]),
            riff(&[fmt.clone(), list.clone(), data.clone()]),
            riff(&[fmt.clone(), data.clone(), list.clone()]),
            riff(&[fmt.clone(), data.clone(), id3.clone()]),
            riff(&[id3.clone(), fmt.clone(), odd.clone(), data.clone(), list]),
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(ok(w).as_str(), GOLDEN_PCM, "wav case {i}");
        }

        let c = comm(2, 3, 16, 44100);
        let s = ssnd(0, &be_samples(&VALS, 2));
        let name = be_chunk(b"NAME", b"abc");
        let anno = be_chunk(b"ANNO", b"abcd");
        let id3a = be_chunk(b"ID3 ", &id3v2(25, false));
        for (i, a) in [
            aiff(b"AIFF", &[name.clone(), c.clone(), s.clone()]),
            aiff(b"AIFF", &[c.clone(), name.clone(), s.clone(), anno.clone()]),
            aiff(b"AIFF", &[c.clone(), s.clone(), id3a.clone()]),
            aiff(b"AIFF", &[id3a, c, anno, s, name]),
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(ok(a).as_str(), GOLDEN_PCM, "aiff case {i}");
        }
    }

    #[test]
    fn ssnd_offset_skips_leading_bytes() {
        let a = aiff(
            b"AIFF",
            &[comm(2, 3, 16, 44100), ssnd(6, &be_samples(&VALS, 2))],
        );
        assert_eq!(ok(&a).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn valid_bits_mask_and_extensible_do_not_matter() {
        let data = chunk(b"data", &le_samples(&VALS, 2));
        for (valid, mask) in [(16, 3), (12, 0), (16, 0x3F), (8, 0xFFFF_FFFF)] {
            let w = riff(&[wav_fmt_ext(2, 44100, 4, 16, valid, mask, 1), data.clone()]);
            assert_eq!(ok(&w).as_str(), GOLDEN_PCM, "valid={valid} mask={mask}");
        }
    }

    // --- difference decisions ---

    #[test]
    fn channel_layout_rate_and_edge_frames_matter() {
        let four = [100, -200, 300, -400];
        let mono = ok(&wav16(1, 44100, &four));
        let stereo = ok(&wav16(2, 44100, &four));
        assert_ne!(mono, stereo);
        assert_eq!(mono.as_str(), spec_pcm_hash(44100, 1, &lj(&four, 16)));

        assert_ne!(ok(&wav16(2, 48000, &VALS)), ok(&wav16(2, 44100, &VALS)));

        let base = ok(&wav16(2, 44100, &VALS));
        let mut lead = vec![0, 0];
        lead.extend(VALS);
        let mut trail = VALS.to_vec();
        trail.extend([0, 0]);
        assert_ne!(ok(&wav16(2, 44100, &lead)), base);
        assert_ne!(ok(&wav16(2, 44100, &trail)), base);
        assert_ne!(ok(&wav16(2, 44100, &lead)), ok(&wav16(2, 44100, &trail)));
        assert_ne!(ok(&aiff16(1, 44100, &four)), ok(&aiff16(2, 44100, &four)));
    }

    // --- strictness ---

    #[test]
    fn partial_trailing_frame_is_malformed() {
        let mut data = le_samples(&VALS, 2);
        data.extend([1, 2, 3]); // 15 bytes, frame is 4
        let w = riff(&[wav_fmt(1, 2, 44100, 4, 16), chunk(b"data", &data)]);
        assert!(is_malformed(pcm(&w)));
        // block align not a multiple of channels
        let w = riff(&[wav_fmt(1, 2, 44100, 3, 16), chunk(b"data", &data)]);
        assert!(is_malformed(pcm(&w)));
    }

    #[test]
    fn ssnd_length_must_match_comm() {
        let long = be_samples(&[1, 2, 3, 4, 5, 6, 7, 8], 2);
        let short = be_samples(&[1, 2, 3], 2);
        for data in [long, short] {
            let a = aiff(b"AIFF", &[comm(2, 3, 16, 44100), ssnd(0, &data)]);
            assert!(is_malformed(pcm(&a)));
        }
        // offset beyond the chunk
        let a = aiff(
            b"AIFF",
            &[
                comm(2, 3, 16, 44100),
                be_chunk(b"SSND", &[0, 0, 1, 0, 0, 0, 0, 0]),
            ],
        );
        assert!(is_malformed(pcm(&a)));
        // missing chunks
        let a = aiff(b"AIFF", &[comm(2, 3, 16, 44100)]);
        assert!(is_malformed(pcm(&a)));
        let a = aiff(b"AIFF", &[ssnd(0, &be_samples(&VALS, 2))]);
        assert!(is_malformed(pcm(&a)));
    }

    #[test]
    fn unsupported_formats() {
        let data = chunk(b"data", &[0u8; 16]);
        // float WAV, by tag and by extensible GUID
        let w = riff(&[wav_fmt(3, 2, 44100, 8, 32), data.clone()]);
        assert!(is_unsupported(pcm(&w)));
        let w = riff(&[wav_fmt_ext(2, 44100, 8, 32, 32, 3, 3), data.clone()]);
        assert!(is_unsupported(pcm(&w)));
        // other tags
        let w = riff(&[wav_fmt(2, 2, 44100, 4, 4), data.clone()]);
        assert!(is_unsupported(pcm(&w)));
        // 3 channels
        let w = riff(&[wav_fmt(1, 3, 44100, 6, 16), data.clone()]);
        assert!(is_unsupported(pcm(&w)));
        let a = aiff(b"AIFF", &[comm(3, 2, 16, 44100), ssnd(0, &[0u8; 12])]);
        assert!(is_unsupported(pcm(&a)));
        // compressed AIFF-C
        for ctype in [b"fl32", b"ulaw", b"ima4"] {
            let a = aiff(
                b"AIFC",
                &[comm_c(2, 2, 16, 44100, ctype), ssnd(0, &[0u8; 8])],
            );
            assert!(is_unsupported(pcm(&a)), "{ctype:?}");
        }
        // 4-byte container is fine, 5-byte is not
        let w = riff(&[wav_fmt(1, 1, 44100, 5, 40), chunk(b"data", &[0u8; 10])]);
        assert!(is_unsupported(pcm(&w)));
        // AIFF sampleSize over 32
        let a = aiff(b"AIFF", &[comm(1, 1, 33, 44100), ssnd(0, &[0u8; 5])]);
        assert!(is_unsupported(pcm(&a)));
    }

    #[test]
    fn non_integer_or_unrepresentable_aiff_rates_are_unsupported() {
        let data = be_samples(&VALS, 2);
        let cases: [[u8; 10]; 7] = [
            // 44100.5
            [0x40, 0x0E, 0xAC, 0x44, 0x80, 0, 0, 0, 0, 0],
            // 0
            [0; 10],
            // negative 44100
            [0xC0, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0],
            // 2^32 (does not fit u32)
            [0x40, 0x1F, 0x80, 0, 0, 0, 0, 0, 0, 0],
            // 0.5
            [0x3F, 0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0],
            // infinity
            [0x7F, 0xFF, 0x80, 0, 0, 0, 0, 0, 0, 0],
            // denormal-ish smallest mantissa
            [0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 1],
        ];
        for (i, rate) in cases.iter().enumerate() {
            let a = aiff(b"AIFF", &[comm_with(2, 3, 16, *rate, &[]), ssnd(0, &data)]);
            assert!(is_unsupported(pcm(&a)), "case {i}");
        }
        // u32::MAX is representable
        let a = aiff(b"AIFF", &[comm(2, 3, 16, u32::MAX), ssnd(0, &data)]);
        assert_eq!(ok(&a).as_str(), spec_pcm_hash(u32::MAX, 2, &lj(&VALS, 16)));
    }

    #[test]
    fn zero_frames_is_no_audio() {
        let w = riff(&[wav_fmt(1, 2, 44100, 4, 16), chunk(b"data", &[])]);
        assert!(is_no_audio(pcm(&w)));
        let a = aiff(b"AIFF", &[comm(2, 0, 16, 44100), ssnd(0, &[])]);
        assert!(is_no_audio(pcm(&a)));
    }

    #[test]
    fn zero_channels_or_rate_are_malformed() {
        let data = chunk(b"data", &[0u8; 8]);
        let w = riff(&[wav_fmt(1, 0, 44100, 4, 16), data.clone()]);
        assert!(is_malformed(pcm(&w)));
        let w = riff(&[wav_fmt(1, 2, 0, 4, 16), data]);
        assert!(is_malformed(pcm(&w)));
        let a = aiff(b"AIFF", &[comm(0, 2, 16, 44100), ssnd(0, &[0u8; 8])]);
        assert!(is_malformed(pcm(&a)));
        let a = aiff(b"AIFF", &[comm(2, 2, 0, 44100), ssnd(0, &[0u8; 8])]);
        assert!(is_malformed(pcm(&a)));
        // short fmt / COMM
        let w = riff(&[chunk(b"fmt ", &[1, 0, 2, 0]), chunk(b"data", &[0u8; 8])]);
        assert!(is_malformed(pcm(&w)));
        let a = aiff(b"AIFF", &[be_chunk(b"COMM", &[0; 10]), ssnd(0, &[0u8; 8])]);
        assert!(is_malformed(pcm(&a)));
    }

    #[test]
    fn unbounded_wav_data_runs_to_eof() {
        let w = riff(&[
            wav_fmt(1, 2, 44100, 4, 16),
            chunk_sized(b"data", 0xFFFF_FFFF, &le_samples(&VALS, 2)),
        ]);
        assert_eq!(ok(&w).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn large_inputs_stream_through_buffers() {
        let n = 100_000;
        let vals: Vec<i32> = (0..n).map(|i| (i * 31) % 60000 - 30000).collect();
        let want = spec_pcm_hash(44100, 2, &lj(&vals, 16));
        assert_eq!(ok(&wav16(2, 44100, &vals)).as_str(), want);
        assert_eq!(ok(&aiff16(2, 44100, &vals)).as_str(), want);
    }

    // --- FLAC ---
    //
    // FLAC is hand-encoded: VERBATIM and CONSTANT subframes, 16-bit, with real
    // CRC-8 / CRC-16 (see `fixtures::flac_frame16`). Real libFLAC fixtures come
    // in a later phase.

    const LEFT: [i32; 3] = [0, 32767, 1];
    const RIGHT: [i32; 3] = [-1, -32768, 256];

    /// STREAMINFO MD5 form: interleaved, signed, LE, right-justified 16-bit.
    fn flac_md5(interleaved: &[i32]) -> [u8; 16] {
        let bytes: Vec<u8> = interleaved
            .iter()
            .flat_map(|s| s.to_le_bytes()[..2].to_vec())
            .collect();
        Md5::digest(&bytes).into()
    }

    fn si_block(channels: u8, bits: u8, total: u64, md5: [u8; 16]) -> (u8, Vec<u8>) {
        (0, flac_streaminfo(44100, channels, bits, total, md5))
    }

    fn golden_frames() -> Vec<u8> {
        flac_frame16(0, 3, &[Sub::Verbatim(&LEFT), Sub::Verbatim(&RIGHT)])
    }

    fn flac_with(total: u64, md5: [u8; 16], frames: &[u8]) -> Vec<u8> {
        flac(&[si_block(2, 16, total, md5)], frames)
    }

    fn golden_flac() -> Vec<u8> {
        flac_with(3, flac_md5(&VALS), &golden_frames())
    }

    #[test]
    fn flac_equals_wav_and_aiff_golden() {
        assert_eq!(ok(&golden_flac()).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn flac_frame_layout_does_not_matter() {
        // Same samples split over two frames of different sizes.
        let two = [
            flac_frame16(
                0,
                2,
                &[Sub::Verbatim(&LEFT[..2]), Sub::Verbatim(&RIGHT[..2])],
            ),
            flac_frame16(
                1,
                1,
                &[Sub::Verbatim(&LEFT[2..]), Sub::Verbatim(&RIGHT[2..])],
            ),
        ]
        .concat();
        assert_eq!(
            ok(&flac_with(3, flac_md5(&VALS), &two)).as_str(),
            GOLDEN_PCM
        );
    }

    #[test]
    fn flac_constant_subframes_equal_wav_silence() {
        let f = flac_with(
            4,
            flac_md5(&[0; 8]),
            &flac_frame16(0, 4, &[Sub::Constant(0), Sub::Constant(0)]),
        );
        assert_eq!(ok(&f), ok(&wav16(2, 44100, &[0; 8])));
        // mono, non-zero constant
        let mono = flac(
            &[si_block(1, 16, 3, flac_md5(&[-5, -5, -5]))],
            &flac_frame16(0, 3, &[Sub::Constant(-5)]),
        );
        assert_eq!(ok(&mono), ok(&wav16(1, 44100, &[-5, -5, -5])));
        assert_ne!(ok(&mono), ok(&wav16(2, 44100, &[-5, -5, -5, -5, -5, -5])));
    }

    #[test]
    fn flac_md5_is_checked_when_set() {
        let good = flac_md5(&VALS);
        assert_eq!(
            ok(&flac_with(3, good, &golden_frames())).as_str(),
            GOLDEN_PCM
        );
        for i in 0..16 {
            let mut bad = good;
            bad[i] ^= 0x01;
            assert!(
                matches!(
                    pcm(&flac_with(3, bad, &golden_frames())),
                    Err(PcmHashError::Md5Mismatch)
                ),
                "byte {i}"
            );
        }
        // all zeros = not set: skipped
        assert_eq!(
            ok(&flac_with(3, [0; 16], &golden_frames())).as_str(),
            GOLDEN_PCM
        );
    }

    #[test]
    fn flac_sample_count_is_checked_unless_zero() {
        let md5 = flac_md5(&VALS);
        for total in [2, 4] {
            // even though the MD5 matches
            assert!(
                matches!(
                    pcm(&flac_with(total, md5, &golden_frames())),
                    Err(PcmHashError::SampleCountMismatch {
                        declared,
                        decoded: 3
                    }) if declared == total
                ),
                "total {total}"
            );
        }
        assert_eq!(
            ok(&flac_with(0, md5, &golden_frames())).as_str(),
            GOLDEN_PCM
        );
    }

    #[test]
    fn flac_corruption_is_a_decode_error() {
        let frames = golden_frames();
        // header byte (CRC-8), sample byte (CRC-16), footer byte (CRC-16)
        for at in [2, 8, 12, frames.len() - 1] {
            let mut bad = frames.clone();
            bad[at] ^= 0x10;
            assert!(
                matches!(
                    pcm(&flac_with(0, [0; 16], &bad)),
                    Err(PcmHashError::Decode(_))
                ),
                "byte {at}"
            );
        }
    }

    #[test]
    fn flac_tags_and_metadata_do_not_matter() {
        let md5 = flac_md5(&VALS);
        let mut tagged = id3v2(40, false);
        tagged.extend(flac(
            &[
                si_block(2, 16, 3, md5),
                padding(300),
                vorbis(30),
                picture(100_000),
            ],
            &golden_frames(),
        ));
        tagged.extend(ape(50, true));
        tagged.extend(id3v1());
        assert_eq!(ok(&tagged).as_str(), GOLDEN_PCM);
    }

    #[test]
    fn flac_streaminfo_must_be_first() {
        let (ty, si) = si_block(2, 16, 3, flac_md5(&VALS));
        let b = flac(&[padding(10), (ty, si)], &golden_frames());
        assert!(is_malformed(pcm(&b)));
        let b = flac(&[vorbis(10)], &golden_frames());
        assert!(is_malformed(pcm(&b)));
    }

    #[test]
    fn flac_streaminfo_disagreeing_with_frames_is_rejected() {
        // channels
        let b = flac(&[si_block(1, 16, 0, [0; 16])], &golden_frames());
        assert!(matches!(pcm(&b), Err(PcmHashError::Decode(_))));
        // samples exceed STREAMINFO bits
        let b = flac(&[si_block(2, 8, 0, [0; 16])], &golden_frames());
        assert!(matches!(pcm(&b), Err(PcmHashError::Decode(_))));
        // more than 2 channels
        let b = flac(&[si_block(3, 16, 0, [0; 16])], &golden_frames());
        assert!(is_unsupported(pcm(&b)));
        // zero sample rate
        let b = flac(
            &[(0, flac_streaminfo(0, 2, 16, 0, [0; 16]))],
            &golden_frames(),
        );
        assert!(is_malformed(pcm(&b)));
    }

    #[test]
    fn flac_without_frames_is_no_audio() {
        assert!(is_no_audio(pcm(&flac(&[si_block(2, 16, 0, [0; 16])], &[]))));
    }

    #[test]
    fn flac_bits_are_left_justified_like_other_containers() {
        // 16-bit FLAC equals the same signal as 24-bit WAV (x256).
        let v24: Vec<i32> = VALS.iter().map(|v| v * 256).collect();
        let w24 = riff(&[
            wav_fmt(1, 2, 44100, 6, 24),
            chunk(b"data", &le_samples(&v24, 3)),
        ]);
        assert_eq!(ok(&golden_flac()), ok(&w24));
    }

    // --- fuzzing ---

    fn valid_inputs() -> Vec<Vec<u8>> {
        vec![
            golden_wav(),
            golden_aiff(),
            golden_flac(),
            flac(
                &[
                    si_block(1, 16, 3, flac_md5(&[-5, -5, -5])),
                    padding(20),
                    vorbis(30),
                ],
                &flac_frame16(0, 3, &[Sub::Constant(-5)]),
            ),
            riff(&[
                chunk(b"LIST", b"INFOabc"),
                wav_fmt_ext(2, 44100, 6, 24, 24, 3, 1),
                chunk(b"data", &le_samples(&VALS, 3)),
                chunk(b"id3 ", &[1; 11]),
            ]),
            riff(&[wav_fmt(1, 1, 8000, 1, 8), chunk(b"data", &[1, 2, 3, 4, 5])]),
            aiff(
                b"AIFC",
                &[
                    comm_c(2, 3, 16, 48000, b"sowt"),
                    be_chunk(b"NAME", b"abc"),
                    ssnd(2, &le_samples(&VALS, 2)),
                ],
            ),
            aiff(
                b"AIFF",
                &[
                    comm(1, 5, 8, 22050),
                    ssnd(0, &[1, 2, 3, 4, 5]),
                    be_chunk(b"ANNO", b"x"),
                ],
            ),
        ]
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn mutated_valid_files_never_panic(
            base in 0usize..8,
            edits in proptest::collection::vec(
                (any::<usize>(), any::<[u8; 8]>(), any::<bool>()),
                0..4,
            ),
            trunc in proptest::option::of(any::<usize>()),
        ) {
            let mut bytes = valid_inputs().swap_remove(base);
            for (at, window, wide) in edits {
                let len = if wide { 8 } else { 4 };
                if bytes.len() >= len {
                    let at = at % (bytes.len() - len + 1);
                    bytes[at..at + len].copy_from_slice(&window[..len]);
                }
            }
            if let Some(t) = trunc {
                let t = t % (bytes.len() + 1);
                bytes.truncate(t);
            }
            // A claxon panic would be caught and reported as DecoderPanic;
            // it must never happen on malformed input.
            let res = pcm_hash_reader(Cursor::new(bytes));
            prop_assert!(!matches!(res, Err(PcmHashError::DecoderPanic)));
        }
    }
}
