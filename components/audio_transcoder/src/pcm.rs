/// Convert f32 sample (normalised to [-1.0, 1.0]) to i16.
///
/// The result is ready for byte-order serialisation:
/// - WAV (little-endian): `f32_to_i16(s).to_le_bytes()`
/// - AIFF (big-endian):   `f32_to_i16(s).to_be_bytes()`
pub fn f32_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 32767.0).round() as i16
}

/// Convert f32 sample (normalised to [-1.0, 1.0]) to i24, stored in an i32.
///
/// Only the lower 24 bits are meaningful. Byte-order serialisation:
/// - WAV (little-endian): take `f32_to_i24(s).to_le_bytes()[..3]`
/// - AIFF (big-endian):   take `f32_to_i24(s).to_be_bytes()[1..]`
pub fn f32_to_i24(sample: f32) -> i32 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 8_388_607.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_i16_max_positive() {
        assert_eq!(f32_to_i16(1.0), 32767);
    }

    #[test]
    fn f32_to_i16_max_negative() {
        assert_eq!(f32_to_i16(-1.0), -32767);
    }

    #[test]
    fn f32_to_i16_zero() {
        assert_eq!(f32_to_i16(0.0), 0);
    }

    #[test]
    fn f32_to_i16_clamps_above_one() {
        assert_eq!(f32_to_i16(2.0), 32767);
    }

    #[test]
    fn f32_to_i16_clamps_below_minus_one() {
        assert_eq!(f32_to_i16(-2.0), -32767);
    }

    #[test]
    fn f32_to_i16_le_bytes_round_trip() {
        let bytes = f32_to_i16(1.0).to_le_bytes();
        let val = i16::from_le_bytes(bytes);
        assert_eq!(val, 32767);
    }

    #[test]
    fn f32_to_i16_be_bytes_round_trip() {
        let bytes = f32_to_i16(1.0).to_be_bytes();
        let val = i16::from_be_bytes(bytes);
        assert_eq!(val, 32767);
    }

    #[test]
    fn f32_to_i16_be_max_negative_round_trip() {
        let bytes = f32_to_i16(-1.0).to_be_bytes();
        let val = i16::from_be_bytes(bytes);
        assert_eq!(val, -32767);
    }

    #[test]
    fn f32_to_i24_max_positive() {
        assert_eq!(f32_to_i24(1.0), 8_388_607);
    }

    #[test]
    fn f32_to_i24_max_negative() {
        assert_eq!(f32_to_i24(-1.0), -8_388_607);
    }

    #[test]
    fn f32_to_i24_zero() {
        assert_eq!(f32_to_i24(0.0), 0);
    }

    #[test]
    fn f32_to_i24_le_bytes_round_trip() {
        let i32_val = f32_to_i24(1.0);
        let bytes = i32_val.to_le_bytes();
        let three = &bytes[..3];
        // sign-extend from 3 bytes little-endian
        let sign_extend = if three[2] & 0x80 != 0 { 0xFF } else { 0x00 };
        let val = i32::from_le_bytes([three[0], three[1], three[2], sign_extend]);
        assert_eq!(val, 8_388_607);
    }

    #[test]
    fn f32_to_i24_be_bytes_round_trip() {
        let i32_val = f32_to_i24(1.0);
        let bytes = i32_val.to_be_bytes();
        let three = &bytes[1..]; // skip the sign-extension byte
        let sign_extend = if three[0] & 0x80 != 0 { 0xFF } else { 0x00 };
        let val = i32::from_be_bytes([sign_extend, three[0], three[1], three[2]]);
        assert_eq!(val, 8_388_607);
    }
}
