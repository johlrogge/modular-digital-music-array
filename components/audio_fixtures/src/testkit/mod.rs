//! Generated AIFF / AIFF-C files for tests (ADR-037). Everything is built in
//! memory from synthetic samples: no binary fixtures, no real audio.
//!
//! ```ignore
//! let bytes = AiffBuilder::new()          // 1 s stereo 16-bit 44.1 kHz tone
//!     .bits(24)
//!     .id3(id3::full_tag(Id3Version::V24))
//!     .chunk(b"MARK", mark_payload())
//!     .build();
//! ```
//!
//! Chunk layout of the output: `FVER` (AIFF-C only), `COMM`, the
//! [`AiffBuilder::chunk`] chunks, `SSND`, then the [`AiffBuilder::trailing_chunk`]
//! and [`AiffBuilder::id3`] chunks in the order they were added. Odd-length
//! payloads get their pad byte.

use crate::{aiff, be_chunk, be_samples, comm, comm_with, ext80, le_samples, ssnd};

pub mod id3;

/// The synthetic signal.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// A sine at this frequency, half of full scale, phase shifted per channel.
    Tone(f64),
    /// Deterministic full-scale pseudo-random noise.
    Noise,
    /// These interleaved samples, `bits` wide (see [`AiffBuilder::pcm`]).
    Samples(Vec<i32>),
}

/// AIFF-C compression type (`COMM` compressionType).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Big-endian integer PCM.
    None,
    /// Big-endian integer PCM.
    Twos,
    /// Little-endian integer PCM.
    Sowt,
    /// 32-bit float. A refusal case.
    Fl32,
    /// mu-law, 8 bits per sample on disk. A refusal case; the bytes are placeholders.
    Ulaw,
}

impl Codec {
    fn fourcc(self) -> &'static [u8; 4] {
        match self {
            Codec::None => b"NONE",
            Codec::Twos => b"twos",
            Codec::Sowt => b"sowt",
            Codec::Fl32 => b"fl32",
            Codec::Ulaw => b"ulaw",
        }
    }
}

/// The FORM type: plain AIFF, or AIFF-C with a compression type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Aiff,
    Aifc(Codec),
}

#[derive(Debug, Clone)]
pub struct AiffBuilder {
    channels: u16,
    bits: u16,
    rate: u32,
    frames: Option<u32>,
    signal: Signal,
    form: Form,
    compression_name: String,
    before_ssnd: Vec<Vec<u8>>,
    after_ssnd: Vec<Vec<u8>>,
}

impl Default for AiffBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AiffBuilder {
    /// One second of a 440 Hz stereo tone, 16-bit, 44.1 kHz, plain AIFF.
    pub fn new() -> Self {
        AiffBuilder {
            channels: 2,
            bits: 16,
            rate: 44_100,
            frames: None,
            signal: Signal::Tone(440.0),
            form: Form::Aiff,
            compression_name: String::new(),
            before_ssnd: Vec::new(),
            after_ssnd: Vec::new(),
        }
    }

    pub fn channels(mut self, channels: u16) -> Self {
        self.channels = channels;
        self
    }

    /// Integer sample depth, 1..=32. Ignored for `Fl32` (32) and `Ulaw` (16 in COMM).
    pub fn bits(mut self, bits: u16) -> Self {
        assert!((1..=32).contains(&bits));
        self.bits = bits;
        self
    }

    pub fn rate(mut self, rate: u32) -> Self {
        self.rate = rate;
        self
    }

    /// Sample frames; the default is one second (`rate` frames).
    pub fn frames(mut self, frames: u32) -> Self {
        self.frames = Some(frames);
        self
    }

    pub fn signal(mut self, signal: Signal) -> Self {
        self.signal = signal;
        self
    }

    pub fn noise(self) -> Self {
        self.signal(Signal::Noise)
    }

    /// Exactly these interleaved samples, each `bits` wide (right-aligned, as
    /// [`AiffBuilder::samples`] returns them). Sets the frame count from the length.
    pub fn pcm(mut self, samples: Vec<i32>) -> Self {
        assert!(self.channels > 0 && samples.len().is_multiple_of(usize::from(self.channels)));
        self.frames = Some((samples.len() / usize::from(self.channels)) as u32);
        self.signal = Signal::Samples(samples);
        self
    }

    /// Make it AIFF-C (form type `AIFC`) with this compression type.
    pub fn aifc(mut self, codec: Codec) -> Self {
        self.form = Form::Aifc(codec);
        self
    }

    /// The human-readable compression name of an AIFF-C `COMM` (a pascal string
    /// after the compression type), empty by default. Writers differ in what they put there.
    pub fn compression_name(mut self, name: &str) -> Self {
        assert!(name.len() < 256);
        self.compression_name = name.to_string();
        self
    }

    /// An extra chunk between `COMM` and `SSND`. The pad byte is added for odd lengths.
    pub fn chunk(mut self, id: &[u8; 4], payload: impl AsRef<[u8]>) -> Self {
        self.before_ssnd.push(be_chunk(id, payload.as_ref()));
        self
    }

    /// An extra chunk after `SSND`.
    pub fn trailing_chunk(mut self, id: &[u8; 4], payload: impl AsRef<[u8]>) -> Self {
        self.after_ssnd.push(be_chunk(id, payload.as_ref()));
        self
    }

    /// An `ID3 ` chunk after `SSND`, carrying `tag` (see [`id3::full_tag`]).
    pub fn id3(self, tag: impl AsRef<[u8]>) -> Self {
        self.trailing_chunk(b"ID3 ", tag)
    }

    fn frame_count(&self) -> u32 {
        self.frames.unwrap_or(self.rate)
    }

    /// Bits per sample of the generated integer samples.
    fn sample_bits(&self) -> u16 {
        match self.form {
            Form::Aifc(Codec::Fl32) => 32,
            _ => self.bits,
        }
    }

    /// The interleaved integer samples the file carries, `bits` wide and
    /// right-aligned (a 20-bit file yields values in 20-bit range). On disk they
    /// are left-justified in their byte container, see [`AiffBuilder::sample_bytes`].
    /// For `Fl32` and `Ulaw` these are the values the bytes derive from.
    pub fn samples(&self) -> Vec<i32> {
        let bits = self.sample_bits();
        let max = ((1i64 << (bits - 1)) - 1) as f64;
        let frames = self.frame_count() as usize;
        let channels = usize::from(self.channels);
        if let Signal::Samples(samples) = &self.signal {
            assert_eq!(samples.len(), frames * channels);
            return samples.clone();
        }
        let mut state = 0x2545_F491_u32;
        let mut out = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            for c in 0..channels {
                let s = match &self.signal {
                    Signal::Tone(hz) => {
                        let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(self.rate)
                            + c as f64 * 0.5;
                        (phase.sin() * 0.5 * max) as i32
                    }
                    _ => {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        (state as i32) >> (32 - bits)
                    }
                };
                out.push(s);
            }
        }
        out
    }

    /// The SSND sample bytes. Integer samples are left-justified in `ceil(bits/8)`
    /// bytes with the low bits zero, as AIFF requires (a 20-bit sample occupies
    /// 3 bytes, shifted left 4).
    pub fn sample_bytes(&self) -> Vec<u8> {
        let samples = self.samples();
        let bytes = usize::from(self.sample_bits().div_ceil(8));
        let justified = || -> Vec<i32> {
            let shift = bytes as u16 * 8 - self.sample_bits();
            samples.iter().map(|s| s << shift).collect()
        };
        match self.form {
            Form::Aifc(Codec::Sowt) => le_samples(&justified(), bytes),
            Form::Aifc(Codec::Fl32) => {
                let scale = (1i64 << 31) as f64;
                samples
                    .iter()
                    .flat_map(|s| ((f64::from(*s) / scale) as f32).to_be_bytes())
                    .collect()
            }
            Form::Aifc(Codec::Ulaw) => samples
                .iter()
                .map(|s| (*s >> (self.bits.max(8) - 8)) as u8)
                .collect(),
            _ => be_samples(&justified(), bytes),
        }
    }

    pub fn build(&self) -> Vec<u8> {
        let frames = self.frame_count();
        let comm = match self.form {
            Form::Aiff => comm(self.channels, frames, self.bits, self.rate),
            Form::Aifc(codec) => {
                let size = match codec {
                    Codec::Fl32 => 32,
                    Codec::Ulaw => 16,
                    _ => self.bits,
                };
                let mut tail = codec.fourcc().to_vec();
                tail.push(self.compression_name.len() as u8); // pstring, padded to even
                tail.extend(self.compression_name.as_bytes());
                if (1 + self.compression_name.len()) % 2 == 1 {
                    tail.push(0);
                }
                comm_with(self.channels, frames, size, ext80(self.rate), &tail)
            }
        };
        let mut chunks = Vec::new();
        if matches!(self.form, Form::Aifc(_)) {
            chunks.push(be_chunk(b"FVER", &0xA280_5140u32.to_be_bytes()));
        }
        chunks.push(comm);
        chunks.extend(self.before_ssnd.iter().cloned());
        chunks.push(ssnd(0, &self.sample_bytes()));
        chunks.extend(self.after_ssnd.iter().cloned());
        let form = match self.form {
            Form::Aiff => b"AIFF",
            Form::Aifc(_) => b"AIFC",
        };
        aiff(form, &chunks)
    }
}

/// `MARK` payload: one marker named "cue" at frame 0.
pub fn mark_payload() -> Vec<u8> {
    let mut p = 1u16.to_be_bytes().to_vec(); // numMarkers
    p.extend(1u16.to_be_bytes()); // id
    p.extend(0u32.to_be_bytes()); // position
    p.extend([3, b'c', b'u', b'e']); // pstring, even total
    p
}

/// `INST` payload (20 bytes): middle C, full key and velocity range, no loops.
pub fn inst_payload() -> Vec<u8> {
    let mut p = vec![60, 0, 0, 127, 1, 127]; // base, detune, low/high note, low/high vel
    p.extend(0i16.to_be_bytes()); // gain
    p.extend([0u8; 12]); // sustain and release loops
    p
}

/// `COMT` payload: one comment, "fixture note!". The payload is 23 bytes, odd, so
/// the chunk gets a pad byte.
pub fn comt_payload() -> Vec<u8> {
    let text = b"fixture note!";
    let mut p = 1u16.to_be_bytes().to_vec(); // numComments
    p.extend(0u32.to_be_bytes()); // timestamp
    p.extend(0u16.to_be_bytes()); // marker id
    p.extend((text.len() as u16).to_be_bytes());
    p.extend(text);
    p
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use lofty::id3::v2::{
        CommentFrame, FrameValue, GeneralEncapsulatedObject, Id3v2Tag, Popularimeter, PrivateFrame,
    };
    use lofty::iff::aiff::AiffFile;
    use lofty::{Accessor, AudioFile, ParseOptions, PictureType, Probe, TaggedFileExt};
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::id3::{self, Id3Version};
    use super::*;

    fn syncsafe_value(b: &[u8]) -> usize {
        b.iter().fold(0usize, |a, x| (a << 7) | usize::from(*x))
    }

    /// `(id, payload_len)` of every top-level chunk, walking the FORM by hand.
    fn chunk_walk(bytes: &[u8]) -> Vec<([u8; 4], usize)> {
        assert_eq!(&bytes[..4], b"FORM");
        let form_len = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(form_len, bytes.len() - 8, "FORM length covers the file");
        let mut at = 12;
        let mut out = Vec::new();
        while at < bytes.len() {
            let id: [u8; 4] = bytes[at..at + 4].try_into().unwrap();
            let len = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            out.push((id, len));
            at += 8 + len + (len % 2);
        }
        assert_eq!(at, bytes.len(), "chunks tile the FORM exactly");
        out
    }

    /// `(id, body_len)` of every frame in an ID3v2 tag.
    fn frame_walk(tag: &[u8]) -> Vec<(String, usize)> {
        assert_eq!(&tag[..3], b"ID3");
        let v4 = tag[3] == 4;
        assert_eq!(syncsafe_value(&tag[6..10]), tag.len() - 10);
        let mut at = 10;
        let mut out = Vec::new();
        while at < tag.len() {
            let id = String::from_utf8(tag[at..at + 4].to_vec()).unwrap();
            let size = &tag[at + 4..at + 8];
            let len = if v4 {
                syncsafe_value(size)
            } else {
                u32::from_be_bytes(size.try_into().unwrap()) as usize
            };
            out.push((id, len));
            at += 10 + len;
        }
        assert_eq!(at, tag.len());
        out
    }

    fn read(bytes: &[u8]) -> lofty::TaggedFile {
        Probe::new(Cursor::new(bytes.to_vec()))
            .guess_file_type()
            .unwrap()
            .read()
            .unwrap()
    }

    #[rstest]
    #[case(8, 1)]
    #[case(8, 2)]
    #[case(16, 1)]
    #[case(16, 2)]
    #[case(24, 1)]
    #[case(24, 2)]
    #[case(32, 1)]
    #[case(32, 2)]
    fn lofty_reads_the_properties(#[case] bits: u16, #[case] channels: u16) {
        let bytes = AiffBuilder::new()
            .bits(bits)
            .channels(channels)
            .rate(48_000)
            .build();
        let props = read(&bytes).properties().clone();
        assert_eq!(props.sample_rate(), Some(48_000));
        assert_eq!(props.channels(), Some(channels as u8));
        assert_eq!(props.bit_depth(), Some(bits as u8));
        assert_eq!(props.duration().as_secs(), 1);
    }

    #[test]
    fn tone_is_not_silent_and_noise_differs_from_tone() {
        let tone = AiffBuilder::new().samples();
        let noise = AiffBuilder::new().noise().samples();
        assert_eq!(tone.len(), 44_100 * 2);
        assert!(tone.iter().any(|s| *s != 0));
        assert!(tone.iter().all(|s| s.abs() <= 16_384));
        assert!(noise.iter().any(|s| s.abs() > 16_384));
        assert!(tone != noise);
    }

    #[test]
    fn pcm_overrides_the_signal_and_sets_the_frame_count() {
        let b = AiffBuilder::new().channels(1).pcm(vec![1, -2, 3]);
        assert_eq!(b.samples(), vec![1, -2, 3]);
        assert_eq!(b.sample_bytes(), vec![0, 1, 0xFF, 0xFE, 0, 3]);
    }

    #[test]
    fn depths_that_are_not_a_byte_multiple_are_left_justified() {
        let b = AiffBuilder::new().bits(20).noise().frames(500);
        let samples = b.samples();
        assert!(samples.iter().all(|s| (-(1 << 19)..(1 << 19)).contains(s)));
        let bytes = b.sample_bytes();
        assert_eq!(bytes.len(), samples.len() * 3);
        for (sample, b) in samples.iter().zip(bytes.chunks(3)) {
            assert_eq!(b[2] & 0x0F, 0, "low nibble of the 3-byte container is zero");
            let stored = i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8;
            assert_eq!(stored, sample << 4);
        }
        assert_eq!(
            read(&b.build()).properties().bit_depth(),
            Some(20),
            "COMM still says 20 bits"
        );
    }

    #[test]
    fn sowt_is_the_byte_swapped_twos() {
        let twos = AiffBuilder::new().aifc(Codec::Twos).frames(100);
        let sowt = AiffBuilder::new().aifc(Codec::Sowt).frames(100);
        let mut swapped = twos.sample_bytes();
        for pair in swapped.chunks_mut(2) {
            pair.reverse();
        }
        assert_eq!(swapped, sowt.sample_bytes());
    }

    #[test]
    fn aifc_carries_fver_before_comm() {
        let bytes = AiffBuilder::new().aifc(Codec::Sowt).build();
        assert_eq!(&bytes[8..12], b"AIFC");
        let ids: Vec<_> = chunk_walk(&bytes).into_iter().map(|c| c.0).collect();
        assert_eq!(ids, vec![*b"FVER", *b"COMM", *b"SSND"]);
    }

    #[test]
    fn extra_chunks_land_where_asked_and_odd_ones_are_padded() {
        let bytes = AiffBuilder::new()
            .frames(100)
            .chunk(b"MARK", mark_payload())
            .chunk(b"INST", inst_payload())
            .chunk(b"COMT", comt_payload())
            .chunk(b"ANNO", b"odd")
            .trailing_chunk(b"ANNO", b"tail")
            .build();
        let walk = chunk_walk(&bytes);
        let ids: Vec<_> = walk.iter().map(|c| c.0).collect();
        assert_eq!(
            ids,
            vec![*b"COMM", *b"MARK", *b"INST", *b"COMT", *b"ANNO", *b"SSND", *b"ANNO"]
        );
        let lens: Vec<_> = walk.iter().map(|c| c.1).collect();
        assert_eq!(lens[1], 2 + 2 + 4 + 4); // MARK
        assert_eq!(lens[2], 20); // INST
        assert_eq!(lens[3], 23, "COMT payload is odd so it exercises the pad");
        assert_eq!(lens[4], 3); // odd ANNO: 3 bytes + 1 pad, tiled exactly by the walk
    }

    #[rstest]
    #[case(Id3Version::V23)]
    #[case(Id3Version::V24)]
    fn full_id3_tag_has_every_frame(#[case] version: Id3Version) {
        let tag = id3::full_tag(version);
        let ids: Vec<String> = frame_walk(&tag).into_iter().map(|f| f.0).collect();
        let year = if version == Id3Version::V23 {
            "TYER"
        } else {
            "TDRC"
        };
        assert_eq!(
            ids,
            vec![
                "TIT2", "TPE1", "TALB", "TCON", year, "TRCK", "TPOS", "TBPM", "TKEY", "COMM",
                "TXXX", "TXXX", "USLT", "POPM", "APIC", "APIC", "GEOB", "PRIV"
            ]
        );
    }

    #[rstest]
    #[case(Id3Version::V23)]
    #[case(Id3Version::V24)]
    fn lofty_reads_the_id3_chunk(#[case] version: Id3Version) {
        let bytes = AiffBuilder::new()
            .frames(1000)
            .chunk(b"MARK", mark_payload())
            .id3(id3::full_tag(version))
            .build();
        let tagged = read(&bytes);
        let tag = tagged.primary_tag().expect("an ID3v2 tag");
        assert_eq!(tag.title().as_deref(), Some(id3::TITLE));
        assert_eq!(tag.artist().as_deref(), Some(id3::ARTIST));
        assert_eq!(tag.album().as_deref(), Some(id3::ALBUM));
        assert_eq!(tag.genre().as_deref(), Some(id3::GENRE));
        assert_eq!(tag.track(), Some(3));
        assert_eq!(tag.disk(), Some(1));
        let pictures = tag.pictures();
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[0].pic_type(), PictureType::CoverFront);
        assert_eq!(pictures[0].data(), id3::TINY_PNG);
        assert_eq!(pictures[1].pic_type(), PictureType::CoverBack);
        assert_eq!(pictures[1].data(), id3::TINY_JPEG);
    }

    fn id3_frames(file: &AiffFile) -> Vec<&FrameValue> {
        let tag: &Id3v2Tag = file.id3v2().expect("an ID3v2 tag");
        tag.into_iter().map(|f| f.content()).collect()
    }

    #[rstest]
    #[case(Id3Version::V23)]
    #[case(Id3Version::V24)]
    fn lofty_decodes_the_binary_and_user_frames(#[case] version: Id3Version) {
        let bytes = AiffBuilder::new()
            .frames(1000)
            .id3(id3::full_tag(version))
            .build();
        let file = AiffFile::read_from(&mut Cursor::new(bytes), ParseOptions::new())
            .expect("lofty reads it");
        let frames = id3_frames(&file);

        let geob: Vec<GeneralEncapsulatedObject> = frames
            .iter()
            .filter_map(|f| match f {
                FrameValue::Binary(b) => GeneralEncapsulatedObject::parse(b).ok(),
                _ => None,
            })
            .collect();
        assert_eq!(geob.len(), 1);
        assert_eq!(geob[0].descriptor.as_deref(), Some(id3::GEOB_DESCRIPTION));
        assert_eq!(geob[0].mime_type.as_deref(), Some(id3::GEOB_MIME));
        assert_eq!(geob[0].data, id3::GEOB_DATA);

        let private: Vec<&PrivateFrame> = frames
            .iter()
            .filter_map(|f| match f {
                FrameValue::Private(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(private.len(), 1);
        assert_eq!(private[0].owner, id3::PRIV_OWNER);
        assert_eq!(private[0].private_data, id3::PRIV_DATA);

        let popm: Vec<&Popularimeter> = frames
            .iter()
            .filter_map(|f| match f {
                FrameValue::Popularimeter(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(popm.len(), 1);
        assert_eq!(popm[0].email, id3::POPM_EMAIL);
        assert_eq!(popm[0].rating, id3::POPM_RATING);
        assert_eq!(popm[0].counter, u64::from(id3::POPM_COUNTER));

        let comments: Vec<&CommentFrame> = frames
            .iter()
            .filter_map(|f| match f {
                FrameValue::Comment(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, id3::COMMENT);

        let user_text: Vec<(&str, &str)> = frames
            .iter()
            .filter_map(|f| match f {
                FrameValue::UserText(t) => Some((t.description.as_str(), t.content.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(user_text, vec![id3::TXXX_1, id3::TXXX_2]);
    }
}
