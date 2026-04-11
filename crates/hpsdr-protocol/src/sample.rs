//! IQ sample codec.
//!
//! Each sample occupies 8 bytes in the wire format:
//!
//! ```text
//! [0..3]  I   — 24-bit signed, big-endian
//! [3..6]  Q   — 24-bit signed, big-endian
//! [6..8]  mic — 16-bit signed, big-endian
//! ```
//!
//! A full receive frame carries 63 of these back-to-back (504 bytes). On
//! the way out (TX) the layout is identical but the 16-bit tail becomes
//! the left/right speaker audio; the 24-bit pair carries the DAC drive.

use core::convert::TryInto;

/// Samples packed into one 512-byte USB frame.
pub const SAMPLES_PER_USB_FRAME: usize = 63;

/// Byte size of one wire-format sample.
pub const SAMPLE_WIRE_LEN: usize = 8;

/// A single IQ sample in host-convenient form.
///
/// `i` and `q` are normalised to `[-1.0, 1.0)` (the 24-bit ADC full scale is
/// `2^23 - 1`). `mic` is left at its native `i16` so that no precision is
/// lost on the way in or out — the DSP chain handles conversion to float at
/// a well-known point rather than here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IqSample {
    pub i: f32,
    pub q: f32,
    pub mic: i16,
}

/// Maximum absolute value of a 24-bit signed integer, used for normalisation.
const I24_SCALE: f32 = 8_388_608.0; // 2^23

impl IqSample {
    /// Decode one wire-format sample from an 8-byte slice.
    ///
    /// Panics if `bytes.len() < 8`. Call sites always slice the right
    /// number of bytes out of a USB frame, so the check is a debug-only
    /// guard rather than a runtime error return.
    pub fn from_bytes(bytes: &[u8; SAMPLE_WIRE_LEN]) -> Self {
        let i_raw = i24_be_to_i32(&bytes[0..3].try_into().unwrap());
        let q_raw = i24_be_to_i32(&bytes[3..6].try_into().unwrap());
        let mic   = i16::from_be_bytes([bytes[6], bytes[7]]);
        IqSample {
            i: (i_raw as f32) / I24_SCALE,
            q: (q_raw as f32) / I24_SCALE,
            mic,
        }
    }

    /// Encode one sample into an 8-byte slice. Values outside `[-1.0, 1.0]`
    /// are clamped to the 24-bit range.
    pub fn to_bytes(self) -> [u8; SAMPLE_WIRE_LEN] {
        let i_raw = f32_to_i24(self.i);
        let q_raw = f32_to_i24(self.q);
        let mut out = [0u8; SAMPLE_WIRE_LEN];
        write_i24_be(&mut out[0..3], i_raw);
        write_i24_be(&mut out[3..6], q_raw);
        out[6..8].copy_from_slice(&self.mic.to_be_bytes());
        out
    }
}

/// Sign-extend a 24-bit big-endian integer to `i32`.
fn i24_be_to_i32(bytes: &[u8; 3]) -> i32 {
    let u = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32);
    // Arithmetic sign extension: shift left to put the sign bit at position
    // 31, then arithmetic-shift right to replicate it.
    ((u << 8) as i32) >> 8
}

/// Clamp a float in `[-1.0, 1.0]` and scale to a 24-bit signed integer.
fn f32_to_i24(v: f32) -> i32 {
    let clamped = v.clamp(-1.0, 1.0 - (1.0 / I24_SCALE));
    (clamped * I24_SCALE) as i32
}

/// Write a 24-bit signed integer as big-endian 3 bytes.
fn write_i24_be(out: &mut [u8], v: i32) {
    debug_assert_eq!(out.len(), 3);
    out[0] = ((v >> 16) & 0xFF) as u8;
    out[1] = ((v >> 8) & 0xFF) as u8;
    out[2] = (v & 0xFF) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i24_sign_extension() {
        assert_eq!(i24_be_to_i32(&[0x00, 0x00, 0x00]), 0);
        assert_eq!(i24_be_to_i32(&[0x00, 0x00, 0x01]), 1);
        assert_eq!(i24_be_to_i32(&[0x7F, 0xFF, 0xFF]), 0x7FFFFF);   // +max
        assert_eq!(i24_be_to_i32(&[0xFF, 0xFF, 0xFF]), -1);
        assert_eq!(i24_be_to_i32(&[0x80, 0x00, 0x00]), -8_388_608); // -max
    }

    #[test]
    fn sample_roundtrip_zero() {
        let s = IqSample { i: 0.0, q: 0.0, mic: 0 };
        let bytes = s.to_bytes();
        assert_eq!(bytes, [0u8; 8]);
        let back = IqSample::from_bytes(&bytes);
        assert_eq!(back, s);
    }

    #[test]
    fn sample_roundtrip_known_values() {
        let bytes = [0x7F, 0xFF, 0xFF,   // I = +max
                     0x80, 0x00, 0x00,   // Q = -max
                     0x12, 0x34];        // mic = 0x1234
        let s = IqSample::from_bytes(&bytes);
        assert!((s.i - (1.0 - 1.0 / I24_SCALE)).abs() < 1e-6);
        assert!((s.q - (-1.0)).abs() < 1e-6);
        assert_eq!(s.mic, 0x1234);

        // Re-encode and require byte-exact round-trip.
        let re = s.to_bytes();
        assert_eq!(re, bytes);
    }

    #[test]
    fn encoding_clamps_out_of_range() {
        let big = IqSample { i: 12.3, q: -5.0, mic: 0 };
        let bytes = big.to_bytes();
        assert_eq!(&bytes[0..3], &[0x7F, 0xFF, 0xFF]);  // +max
        assert_eq!(&bytes[3..6], &[0x80, 0x00, 0x00]);  // -max
    }
}
