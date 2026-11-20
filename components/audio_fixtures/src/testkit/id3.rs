//! Hand-encoded ID3v2.3 / ID3v2.4 tags for AIFF fixtures.

use crate::syncsafe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id3Version {
    V23,
    V24,
}

impl Id3Version {
    fn major(self) -> u8 {
        match self {
            Id3Version::V23 => 3,
            Id3Version::V24 => 4,
        }
    }

    /// Text encoding byte: UTF-16 with BOM in v2.3 (no UTF-8 there), UTF-8 in v2.4.
    fn encoding(self) -> u8 {
        match self {
            Id3Version::V23 => 1,
            Id3Version::V24 => 3,
        }
    }

    fn text(self, s: &str) -> Vec<u8> {
        match self {
            Id3Version::V23 => {
                let mut v = vec![0xFF, 0xFE];
                v.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
                v
            }
            Id3Version::V24 => s.as_bytes().to_vec(),
        }
    }

    fn terminator(self) -> &'static [u8] {
        match self {
            Id3Version::V23 => &[0, 0],
            Id3Version::V24 => &[0],
        }
    }

    /// Encoded string followed by its terminator. An empty v2.3 string still
    /// carries its BOM, so every UTF-16 string is self-describing.
    fn text_z(self, s: &str) -> Vec<u8> {
        let mut v = self.text(s);
        v.extend(self.terminator());
        v
    }
}

pub const TITLE: &str = "Fixture Title";
pub const ARTIST: &str = "Fixture Artist";
pub const ALBUM: &str = "Fixture Album";
pub const GENRE: &str = "Techno";
pub const YEAR: &str = "2024";
pub const TRACK: &str = "3/12";
pub const DISC: &str = "1/2";
pub const BPM: &str = "128";
pub const KEY: &str = "Am";
pub const COMMENT: &str = "Fixture comment";
pub const LYRICS: &str = "la la la";
pub const TXXX_1: (&str, &str) = ("MOOD", "dark");
pub const TXXX_2: (&str, &str) = ("ENERGY", "7");
pub const GEOB_DESCRIPTION: &str = "Serato Markers_";
pub const GEOB_MIME: &str = "application/octet-stream";
pub const GEOB_DATA: &[u8] = &[0x01, 0x01, 0x00, 0xFF, 0x00, 0xFE, 0x7F, 0x80, 0x00, 0x42];
pub const PRIV_OWNER: &str = "TRAKTOR4";
pub const PRIV_DATA: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
pub const POPM_EMAIL: &str = "fixture@example.com";
pub const POPM_RATING: u8 = 196;
pub const POPM_COUNTER: u32 = 5;

/// A valid 1x1 PNG.
pub const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// A minimal JPEG-shaped byte string (SOI, JFIF APP0, EOI); not decodable.
pub const TINY_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
];

/// One ID3v2 frame: 10-byte header (flags zero) and body.
pub fn frame(version: Id3Version, id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    match version {
        Id3Version::V23 => v.extend((body.len() as u32).to_be_bytes()),
        Id3Version::V24 => v.extend(syncsafe(body.len() as u32)),
    }
    v.extend([0, 0]);
    v.extend(body);
    v
}

/// A text frame (`TIT2`, `TPE1`, ...).
pub fn text_frame(version: Id3Version, id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(version.text(text));
    frame(version, id, &body)
}

pub fn comm_frame(version: Id3Version, text: &str) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(b"eng");
    body.extend(version.text_z("")); // empty short description
    body.extend(version.text(text));
    frame(version, b"COMM", &body)
}

pub fn txxx_frame(version: Id3Version, description: &str, value: &str) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(version.text_z(description));
    body.extend(version.text(value));
    frame(version, b"TXXX", &body)
}

pub fn uslt_frame(version: Id3Version, lyrics: &str) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(b"eng");
    body.extend(version.text_z("")); // empty content descriptor
    body.extend(version.text(lyrics));
    frame(version, b"USLT", &body)
}

pub fn popm_frame(version: Id3Version, email: &str, rating: u8, counter: u32) -> Vec<u8> {
    let mut body = email.as_bytes().to_vec();
    body.push(0);
    body.push(rating);
    body.extend(counter.to_be_bytes());
    frame(version, b"POPM", &body)
}

/// `picture_type` is the ID3 code: 3 front cover, 4 back cover.
pub fn apic_frame(version: Id3Version, mime: &str, picture_type: u8, data: &[u8]) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(mime.as_bytes());
    body.push(0);
    body.push(picture_type);
    body.extend(version.text_z("")); // empty description
    body.extend(data);
    frame(version, b"APIC", &body)
}

pub fn geob_frame(version: Id3Version, description: &str, data: &[u8]) -> Vec<u8> {
    let mut body = vec![version.encoding()];
    body.extend(GEOB_MIME.as_bytes());
    body.push(0);
    body.extend(version.text_z("")); // empty file name
    body.extend(version.text_z(description));
    body.extend(data);
    frame(version, b"GEOB", &body)
}

pub fn priv_frame(version: Id3Version, owner: &str, data: &[u8]) -> Vec<u8> {
    let mut body = owner.as_bytes().to_vec();
    body.push(0);
    body.extend(data);
    frame(version, b"PRIV", &body)
}

/// The frame set the ingest tests expect. v2.3 has no `TDRC`; the year goes in
/// `TYER` there, as real v2.3 taggers write it.
pub fn full_frames(version: Id3Version) -> Vec<Vec<u8>> {
    let year_id: &[u8; 4] = match version {
        Id3Version::V23 => b"TYER",
        Id3Version::V24 => b"TDRC",
    };
    vec![
        text_frame(version, b"TIT2", TITLE),
        text_frame(version, b"TPE1", ARTIST),
        text_frame(version, b"TALB", ALBUM),
        text_frame(version, b"TCON", GENRE),
        text_frame(version, year_id, YEAR),
        text_frame(version, b"TRCK", TRACK),
        text_frame(version, b"TPOS", DISC),
        text_frame(version, b"TBPM", BPM),
        text_frame(version, b"TKEY", KEY),
        comm_frame(version, COMMENT),
        txxx_frame(version, TXXX_1.0, TXXX_1.1),
        txxx_frame(version, TXXX_2.0, TXXX_2.1),
        uslt_frame(version, LYRICS),
        popm_frame(version, POPM_EMAIL, POPM_RATING, POPM_COUNTER),
        apic_frame(version, "image/png", 3, TINY_PNG),
        apic_frame(version, "image/jpeg", 4, TINY_JPEG),
        geob_frame(version, GEOB_DESCRIPTION, GEOB_DATA),
        priv_frame(version, PRIV_OWNER, PRIV_DATA),
    ]
}

/// A complete ID3v2 tag (header + frames), the payload of an `ID3 ` chunk.
pub fn tag(version: Id3Version, frames: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let mut v = b"ID3".to_vec();
    v.extend([version.major(), 0, 0]);
    v.extend(syncsafe(body.len() as u32));
    v.extend(body);
    v
}

/// The tag with every frame in [`full_frames`].
pub fn full_tag(version: Id3Version) -> Vec<u8> {
    tag(version, &full_frames(version))
}
