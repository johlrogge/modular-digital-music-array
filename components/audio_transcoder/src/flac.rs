use std::io::Write;

use crate::{TranscodeParams, TranscoderError};

/// FLAC encoding is not yet supported via this writer path.
///
/// If the source is already a FLAC file, the caller should copy the raw blob
/// directly rather than going through the transcode pipeline. A real FLAC
/// encoder can be wired in later (e.g., via `claxon` or a native encoder).
pub fn write_flac(
    _writer: &mut impl Write,
    _params: &TranscodeParams,
    _samples: &[f32],
) -> Result<(), TranscoderError> {
    Err(TranscoderError::FlacEncodingNotSupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BitDepth, ExportFormat, TranscodeParams};

    #[test]
    fn flac_write_returns_not_supported_error() {
        let params = TranscodeParams {
            format: ExportFormat::Flac,
            channels: 2,
            sample_rate: 44100,
            bit_depth: BitDepth::Sixteen,
        };
        let samples: Vec<f32> = vec![0.0; 100];
        let mut buf = Vec::new();
        let result = write_flac(&mut buf, &params, &samples);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            TranscoderError::FlacEncodingNotSupported
        ));
    }
}
