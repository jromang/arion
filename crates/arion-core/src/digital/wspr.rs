//! WSPR — Weak Signal Propagation Reporter.
//!
//! Slot-based mode: a WSPR transmission is exactly **162 symbols**
//! at 1.46 bd = 110.6 seconds of signal inside a 120-second UTC
//! slot (xx:00, xx:02, …). At 12 kHz the full slot is
//! ~1 440 000 complex samples; post-downsampling to the canonical
//! 375 Hz WSPR rate it is 45 000 samples.
//!
//! This module currently holds the **slot accumulator** — the
//! resampler, the 120-second UTC boundary detector, and the stage
//! wiring into `DigitalPipeline`. The decoder itself (Fano-based
//! convolutional decode of the 50-bit payload to callsign, locator
//! and power) is a separate large chunk of C that needs to be
//! vendored from WSJT-X (lib/wsprd/) and wrapped as `wsprd-sys`,
//! tracked in todo/other_modes.md.
//!
//! Until that lands `decode_slot` returns an empty vector, but the
//! rest of the pipeline (sample flow, slot timing, UI surfacing)
//! works end-to-end, so swapping in a real decoder is a one-file
//! change.

use liquid::{Complex32, MsResamp};

use super::DigitalDecode;

/// Canonical WSPR baseband rate. 12 kHz input is decimated to this
/// before any symbol-level processing.
pub const WSPR_RATE_HZ: f32 = 375.0;
pub const WSPR_SLOT_SECS: u64 = 120;

/// One decoded WSPR frame.
#[derive(Debug, Clone)]
pub struct WsprDecode {
    pub callsign: String,
    pub locator: String,
    pub power_dbm: i8,
    pub snr_db: f32,
    pub freq_hz: f32,
    pub time_offset_s: f32,
}

pub struct WsprDecoder {
    /// 48 kHz real → ~375 Hz complex baseband.
    resampler: MsResamp,
    scratch_in: Vec<Complex32>,
    scratch_out: Vec<Complex32>,
    /// Accumulated baseband samples for the current slot.
    samples: Vec<Complex32>,
    /// Last UTC slot index we saw; used to detect the xx:00 / xx:02
    /// boundary that triggers decode+reset.
    last_slot_idx: Option<u64>,
}

fn current_wspr_slot_idx() -> Option<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() / WSPR_SLOT_SECS)
}

impl WsprDecoder {
    pub fn new() -> Option<Self> {
        let resampler = MsResamp::new(WSPR_RATE_HZ / 48_000.0, 60.0).ok()?;
        Some(Self {
            resampler,
            scratch_in: Vec::with_capacity(2048),
            scratch_out: Vec::with_capacity(64),
            samples: Vec::with_capacity(45_000 + 512),
            last_slot_idx: current_wspr_slot_idx(),
        })
    }

    /// Feed a block of post-AGC real audio (48 kHz). When the UTC
    /// 120-second slot boundary ticks over, decode what we have and
    /// return the resulting frames.
    pub fn push_audio(&mut self, audio: &[f32]) -> Vec<WsprDecode> {
        self.scratch_in.clear();
        self.scratch_in.reserve(audio.len());
        for &s in audio {
            self.scratch_in.push(Complex32 { re: s, im: 0.0 });
        }
        let cap = self.resampler.num_output(audio.len() as u32) as usize + 16;
        if self.scratch_out.len() < cap {
            self.scratch_out.resize(cap, Complex32 { re: 0.0, im: 0.0 });
        }
        let n = self
            .resampler
            .execute(&self.scratch_in, &mut self.scratch_out);
        self.samples.extend_from_slice(&self.scratch_out[..n]);

        let slot_now = current_wspr_slot_idx();
        let boundary = matches!(
            (slot_now, self.last_slot_idx),
            (Some(now), Some(last)) if now != last
        );
        if boundary {
            let decodes = decode_slot(&self.samples);
            self.samples.clear();
            if let Some(idx) = slot_now {
                self.last_slot_idx = Some(idx);
            }
            decodes
        } else {
            Vec::new()
        }
    }
}

/// Decode a full 120-s WSPR slot from downsampled (375 Hz complex)
/// baseband samples.
///
/// TODO(wsprd-sys): wire in the vendored ft8_lib-style safe wrapper
/// around WSJT-X's wsprd decoder. For now the function returns no
/// decodes so the pipeline compiles and the UI surfaces the mode.
fn decode_slot(_samples: &[Complex32]) -> Vec<WsprDecode> {
    Vec::new()
}

/// Convert a [`WsprDecode`] to a [`DigitalDecode`] for the pipeline.
pub fn to_digital_decode(d: &WsprDecode) -> DigitalDecode {
    DigitalDecode {
        mode: super::DigitalMode::Wspr,
        text: format!("{} {} {}dBm", d.callsign, d.locator, d.power_dbm),
        snr_db: d.snr_db,
        freq_hz: d.freq_hz,
        time_offset_s: d.time_offset_s,
    }
}
