//! The FLAC's tags: Vorbis comments and PICTURE blocks, written from the AIFF's
//! tag (ADR-037 decision 3). They are a view for tools; the lossless record is
//! the `aiff` APPLICATION block libFLAC wrote.

use std::path::Path;

use audio_metadata::{read_tag, MetadataError};
use lofty::error::ErrorKind;
use lofty::{ItemKey, PictureType, Tag, TagExt, TagType};

use crate::refusal::{EncodeError, EnvironmentError, Refusal, RefusalReason};

/// Whether reading a tag failed because of I/O, not because of what is in the
/// file. Parse errors say something about the file; I/O errors do not.
pub(crate) fn is_io(e: &MetadataError) -> bool {
    match e {
        MetadataError::Io(_) => true,
        MetadataError::Lofty(l) => matches!(l.kind(), ErrorKind::Io(_)),
        MetadataError::NotFound(_) | MetadataError::ParseError(_) => false,
    }
}

/// An I/O failure on a file this encoder wrote.
fn staged_io(path: &Path, e: impl std::fmt::Display) -> EncodeError {
    EnvironmentError::StagedFile(format!("{}: {e}", path.display())).into()
}

/// Read the staged FLAC's tag only to classify a failure: I/O is the machine's
/// (`StagedFile`); anything else is left to the caller's own reading.
pub(crate) fn check_staged_readable(flac: &Path) -> Result<(), EncodeError> {
    match read_tag(flac) {
        Err(e) if is_io(&e) => Err(staged_io(flac, e)),
        _ => Ok(()),
    }
}

/// A picture as the acceptance check compares it: type, MIME and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PictureKey {
    pic_type: PictureType,
    mime: Option<String>,
    data: Vec<u8>,
}

/// `tag` as Vorbis comments. ID3v2 `TBPM` is lofty's `IntegerBpm`, which has no
/// Vorbis counterpart and would be dropped; Vorbis calls it `BPM`.
fn vorbis_view(mut tag: Tag) -> Tag {
    let integer_bpm = tag.get_string(&ItemKey::IntegerBpm).map(str::to_owned);
    tag.re_map(TagType::VorbisComments);
    if let Some(bpm) = integer_bpm {
        if tag.get(&ItemKey::Bpm).is_none() {
            tag.insert_text(ItemKey::Bpm, bpm);
        }
    }
    tag
}

/// Write the Vorbis comments and every PICTURE of `src`'s tag into `flac`.
/// Other metadata blocks of the FLAC, the `aiff` APPLICATION block among them,
/// are left alone.
pub(crate) fn write_from(src: &Path, flac: &Path) -> Result<(), EncodeError> {
    let tag = read_tag(src)
        .map_err(|e| Refusal::new(RefusalReason::TagWriteFailed, format!("reading tag: {e}")))?;
    let Some(tag) = tag else {
        return Ok(());
    };
    vorbis_view(tag).save_to_path(flac).map_err(|e| {
        if matches!(e.kind(), ErrorKind::Io(_)) {
            staged_io(flac, e)
        } else {
            Refusal::new(RefusalReason::TagWriteFailed, e).into()
        }
    })
}

/// The pictures of the file's tag, in order. `staged` says the file is one this
/// encoder wrote: an I/O failure on it is the machine's, on the source it is
/// the source's.
pub(crate) fn pictures(path: &Path, staged: bool) -> Result<Vec<PictureKey>, EncodeError> {
    let tag = read_tag(path).map_err(|e| match (is_io(&e), staged) {
        (true, true) => staged_io(path, e),
        (true, false) => Refusal::new(RefusalReason::UnreadableSource, &e).into(),
        (false, _) => Refusal::new(
            RefusalReason::PictureMismatch,
            format!("reading pictures of {}: {e}", path.display()),
        )
        .into(),
    })?;
    Ok(tag
        .map(|tag| {
            tag.pictures()
                .iter()
                .map(|p| PictureKey {
                    pic_type: p.pic_type(),
                    mime: p.mime_type().map(|m| m.as_str().to_string()),
                    data: p.data().to_vec(),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Count, types, MIME types and bytes of the pictures are equal.
pub(crate) fn check_pictures(aiff: &Path, flac: &Path) -> Result<(), EncodeError> {
    let want = pictures(aiff, false)?;
    let got = pictures(flac, true)?;
    if want.len() != got.len() {
        return Err(Refusal::new(
            RefusalReason::PictureMismatch,
            format!("{} in the AIFF, {} in the FLAC", want.len(), got.len()),
        )
        .into());
    }
    for (i, (w, g)) in want.iter().zip(&got).enumerate() {
        if w.pic_type != g.pic_type || w.mime != g.mime {
            return Err(Refusal::new(
                RefusalReason::PictureMismatch,
                format!(
                    "picture {i}: AIFF {:?} {:?}, FLAC {:?} {:?}",
                    w.pic_type, w.mime, g.pic_type, g.mime
                ),
            )
            .into());
        }
        if w.data != g.data {
            return Err(Refusal::new(
                RefusalReason::PictureMismatch,
                format!("picture {i}: the image bytes differ"),
            )
            .into());
        }
    }
    Ok(())
}
