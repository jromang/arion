//! IQ sample codec.
//!
//! # Single-RX layout (`num_rx == 1`, the phase-A default)
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
//!
//! # Multi-RX layout (`num_rx > 1`)
//!
//! When more than one receiver is enabled, the IQ pairs of every RX are
//! interleaved in each wire sample *before* the mic audio:
//!
//! ```text
//! num_rx=2: I1 Q1 I2 Q2 mic                (14 bytes/sample, 36 samples/frame)
//! num_rx=3: I1 Q1 I2 Q2 I3 Q3 mic          (20 bytes/sample, 25 samples/frame)
//! num_rx=N: I1 Q1 ... IN QN mic            (2 + 6*N bytes/sample)
//! ```
//!
//! [`sample_wire_stride`] and [`samples_per_usb_frame_n`] compute the
//! exact values for any receiver count. Because `2 + 6*N` generally
//! doesn't divide 504 evenly, the tail of each USB frame has a few
//! unused bytes the host MUST ignore on the way in and MUST zero on the
//! way out — see the table on [`samples_per_usb_frame_n`].

use core::convert::TryInto;

/// Maximum number of simultaneous receivers any HPSDR Protocol 1 host
/// in this codebase will ever advertise. HL2 only goes up to 2, Saturn
/// / ANAN-G2 go up to 7. `8` gives us one extra slot of headroom.
pub const MAX_RX: usize = 8;

/// Samples packed into one 512-byte USB frame when `num_rx == 1`
/// (back-compat alias for [`samples_per_usb_frame_n(1)`]).
pub const SAMPLES_PER_USB_FRAME: usize = 63;

/// Byte size of one wire-format sample when `num_rx == 1`. See
/// [`sample_wire_stride`] for the general form.
pub const SAMPLE_WIRE_LEN: usize = 8;

/// Byte size of one wire-format sample at the given receiver count.
///
/// `2 + 6 * num_rx` — 2 bytes of mic audio plus 6 bytes per RX
/// (3-byte I and 3-byte Q at 24-bit depth).
pub const fn sample_wire_stride(num_rx: usize) -> usize {
    2 + 6 * num_rx
}

/// How many samples fit in one 504-byte USB frame payload at the given
/// receiver count.
///
/// ```text
/// num_rx=1 :  63 samples/frame (0 bytes unused)
/// num_rx=2 :  36 samples/frame (0 bytes unused)
/// num_rx=3 :  25 samples/frame (4 bytes unused)
/// num_rx=4 :  19 samples/frame (10 bytes unused)
/// num_rx=5 :  15 samples/frame (24 bytes unused)
/// num_rx=6 :  13 samples/frame (10 bytes unused)
/// num_rx=7 :  11 samples/frame (20 bytes unused)
/// num_rx=8 :  10 samples/frame (4 bytes unused)
/// ```
pub const fn samples_per_usb_frame_n(num_rx: usize) -> usize {
    // 504 = SAMPLES_SECTION_LEN; can't import from metis here without a
    // circular module dep, so hardcoded with a debug_assert elsewhere.
    504 / sample_wire_stride(num_rx)
}

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

/// A wire sample with I/Q for up to [`MAX_RX`] receivers plus one mic audio.
///
/// `rx[0..num_rx as usize]` holds live data; entries past `num_rx` are
/// zero-initialised and must not be read. All `rx[r]` I/Q values are
/// normalised to `[-1.0, 1.0)`.
///
/// For the common `num_rx == 1` case, [`IqSample`] is a faster
/// single-purpose alternative. `MultiIqSample` exists for multi-RX
/// operation where the wire stride depends on `num_rx`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MultiIqSample {
    pub rx:     [(f32, f32); MAX_RX],
    pub num_rx: u8,
    pub mic:    i16,
}

impl Default for MultiIqSample {
    fn default() -> Self {
        MultiIqSample {
            rx:     [(0.0, 0.0); MAX_RX],
            num_rx: 1,
            mic:    0,
        }
    }
}

impl MultiIqSample {
    /// Decode one multi-RX wire sample. `bytes.len()` must equal
    /// [`sample_wire_stride(num_rx)`]; violating that is a programmer
    /// error, not a runtime error.
    pub fn from_bytes(bytes: &[u8], num_rx: usize) -> Self {
        debug_assert!((1..=MAX_RX).contains(&num_rx));
        debug_assert_eq!(bytes.len(), sample_wire_stride(num_rx));

        let mut rx = [(0.0_f32, 0.0_f32); MAX_RX];
        for (r, slot) in rx.iter_mut().enumerate().take(num_rx) {
            let off = r * 6;
            let i_raw = i24_be_to_i32(&bytes[off..off + 3].try_into().unwrap());
            let q_raw = i24_be_to_i32(&bytes[off + 3..off + 6].try_into().unwrap());
            *slot = (
                (i_raw as f32) / I24_SCALE,
                (q_raw as f32) / I24_SCALE,
            );
        }
        let mic_off = num_rx * 6;
        let mic = i16::from_be_bytes([bytes[mic_off], bytes[mic_off + 1]]);
        MultiIqSample {
            rx,
            num_rx: num_rx as u8,
            mic,
        }
    }

    /// Encode this sample into `bytes`. `bytes.len()` must equal
    /// [`sample_wire_stride(self.num_rx)`].
    pub fn to_bytes(self, bytes: &mut [u8]) {
        let n = self.num_rx as usize;
        debug_assert!((1..=MAX_RX).contains(&n));
        debug_assert_eq!(bytes.len(), sample_wire_stride(n));

        for (r, &(i, q)) in self.rx.iter().enumerate().take(n) {
            let off = r * 6;
            write_i24_be(&mut bytes[off..off + 3], f32_to_i24(i));
            write_i24_be(&mut bytes[off + 3..off + 6], f32_to_i24(q));
        }
        let mic_off = n * 6;
        bytes[mic_off..mic_off + 2].copy_from_slice(&self.mic.to_be_bytes());
    }
}

/// Maximum absolute value of a 24-bit signed integer, used for normalisation.
const I24_SCALE: f32 = 8_388_608.0; // 2^23

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

    // --- Multi-RX layout -----------------------------------------------

    #[test]
    fn wire_stride_table() {
        assert_eq!(sample_wire_stride(1), 8);
        assert_eq!(sample_wire_stride(2), 14);
        assert_eq!(sample_wire_stride(3), 20);
        assert_eq!(sample_wire_stride(4), 26);
        assert_eq!(sample_wire_stride(8), 50);
    }

    #[test]
    fn samples_per_frame_table() {
        assert_eq!(samples_per_usb_frame_n(1), 63);
        assert_eq!(samples_per_usb_frame_n(2), 36);
        assert_eq!(samples_per_usb_frame_n(3), 25);
        assert_eq!(samples_per_usb_frame_n(4), 19);
        assert_eq!(samples_per_usb_frame_n(5), 15);
        assert_eq!(samples_per_usb_frame_n(6), 13);
        assert_eq!(samples_per_usb_frame_n(7), 11);
        assert_eq!(samples_per_usb_frame_n(8), 10);
    }

    #[test]
    fn multi_sample_roundtrip_num_rx_2() {
        let mut original = MultiIqSample {
            num_rx: 2,
            mic:    0x1234,
            ..MultiIqSample::default()
        };
        original.rx[0] = (0.25, -0.5);
        original.rx[1] = (0.75, 0.125);

        let mut buf = [0u8; 14];
        original.to_bytes(&mut buf);
        let decoded = MultiIqSample::from_bytes(&buf, 2);

        assert_eq!(decoded.num_rx, 2);
        assert_eq!(decoded.mic, 0x1234);
        assert!((decoded.rx[0].0 - 0.25).abs() < 1e-6);
        assert!((decoded.rx[0].1 + 0.5 ).abs() < 1e-6);
        assert!((decoded.rx[1].0 - 0.75).abs() < 1e-6);
        assert!((decoded.rx[1].1 - 0.125).abs() < 1e-6);
        // Unused slots must remain zeroed by the decoder.
        assert_eq!(decoded.rx[2], (0.0, 0.0));
    }

    #[test]
    fn multi_sample_roundtrip_num_rx_3() {
        let mut original = MultiIqSample {
            num_rx: 3,
            mic:    -1000,
            ..MultiIqSample::default()
        };
        original.rx[0] = (0.1, 0.2);
        original.rx[1] = (0.3, 0.4);
        original.rx[2] = (0.5, 0.6);

        let mut buf = [0u8; 20];
        original.to_bytes(&mut buf);
        let decoded = MultiIqSample::from_bytes(&buf, 3);

        for r in 0..3 {
            let (oi, oq) = original.rx[r];
            let (di, dq) = decoded.rx[r];
            assert!((oi - di).abs() < 1e-6, "rx[{r}].i mismatch");
            assert!((oq - dq).abs() < 1e-6, "rx[{r}].q mismatch");
        }
        assert_eq!(decoded.mic, -1000);
    }

    #[test]
    fn multi_sample_num_rx_1_matches_iq_sample() {
        // For num_rx=1 the wire format is identical to IqSample's.
        // Encode the same values through both paths and verify the
        // byte buffers match.
        let mono = IqSample { i: 0.3, q: -0.7, mic: 42 };
        let mono_bytes = mono.to_bytes();

        let mut multi = MultiIqSample {
            num_rx: 1,
            mic:    42,
            ..MultiIqSample::default()
        };
        multi.rx[0] = (0.3, -0.7);
        let mut multi_bytes = [0u8; 8];
        multi.to_bytes(&mut multi_bytes);

        assert_eq!(mono_bytes, multi_bytes);
    }
}
