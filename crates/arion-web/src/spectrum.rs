//! Binary spectrum frame encoder.
//!
//! Layout (little-endian):
//!
//! ```text
//!   offset  size  field
//!   0       1     tag = 0x01 (spectrum)
//!   1       1     rx_idx
//!   2       4     center_freq_hz (u32)
//!   6       4     span_hz        (u32)
//!   10      2     nbins          (u16)
//!   12      4*N   bins (f32 dB)
//! ```

use arion_core::RxTelemetry;

pub const TAG_SPECTRUM: u8 = 0x01;

pub fn encode(rx_idx: u8, rt: &RxTelemetry) -> Vec<u8> {
    let n = rt.spectrum_bins_db.len() as u16;
    let mut buf = Vec::with_capacity(12 + 4 * n as usize);
    buf.push(TAG_SPECTRUM);
    buf.push(rx_idx);
    buf.extend_from_slice(&rt.center_freq_hz.to_le_bytes());
    buf.extend_from_slice(&rt.span_hz.to_le_bytes());
    buf.extend_from_slice(&n.to_le_bytes());
    for &v in &rt.spectrum_bins_db {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}
