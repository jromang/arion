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

use std::sync::Arc;

use liquid::{Complex32, MsResamp};
use rustfft::{num_complex::Complex, Fft, FftPlanner};

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
/// **Detector, not decoder.** Until `wsprd-sys` exposes a library
/// API for WSJT-X's wsprd (see `todo/other_modes.md`), this scans
/// the slot spectrum for a narrow peak (characteristic of the
/// ~6 Hz-wide WSPR signal). When a peak stands clear of the noise
/// floor we emit a `WsprDecode` with `callsign = "SIGNAL"` and the
/// detected frequency / SNR — enough to confirm the pipeline is
/// hearing something while we wait on real Fano decoding.
fn decode_slot(samples: &[Complex32]) -> Vec<WsprDecode> {
    const FFT_LEN: usize = 32_768;
    // White noise of length 32 768 routinely shows a max/10%-ile
    // ratio ≈ 20 dB just from statistics; we need headroom above
    // that to keep false positives out.
    const DETECT_SNR_DB: f32 = 28.0;

    if samples.len() < FFT_LEN {
        return Vec::new();
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft: Arc<dyn Fft<f32>> = planner.plan_fft_forward(FFT_LEN);
    let mut buf: Vec<Complex<f32>> = samples[..FFT_LEN]
        .iter()
        .map(|c| Complex::new(c.re, c.im))
        .collect();
    fft.process(&mut buf);

    let mut mags: Vec<f32> = buf.iter().map(|c| c.norm_sqr()).collect();
    // Zero the DC bin — real radios leak a strong spike there
    // (mixer offset) that has nothing to do with the WSPR signal.
    mags[0] = 0.0;

    // Peak bin
    let (peak_bin, &peak_mag) = mags
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("FFT_LEN > 0");

    // Noise floor ≈ 10th-percentile magnitude. More robust than
    // the median against a strong but narrow peak + its FFT
    // sidelobes, and against broadband noise being eaten by an
    // in-band interferer.
    let mut sorted = mags;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 10].max(1e-20);
    let snr_db = 10.0 * (peak_mag / floor).log10();

    if snr_db < DETECT_SNR_DB {
        return Vec::new();
    }

    // Map FFT bin → baseband Hz. rustfft produces the DC bin at 0
    // and Nyquist at FFT_LEN/2; bins above that are negative
    // frequencies in a complex input. Fold them into ±bin_hz.
    let bin_hz = WSPR_RATE_HZ / FFT_LEN as f32;
    let signed_bin = if peak_bin > FFT_LEN / 2 {
        peak_bin as i32 - FFT_LEN as i32
    } else {
        peak_bin as i32
    };
    let freq_hz = signed_bin as f32 * bin_hz;

    vec![WsprDecode {
        callsign: "SIGNAL".into(),
        locator: "?".into(),
        power_dbm: 0,
        snr_db,
        freq_hz,
        time_offset_s: 0.0,
    }]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic 5 Hz tone buried in white noise should be detected
    /// (SNR ~ +40 dB in the FFT bin).
    #[test]
    fn detects_narrow_tone_above_noise() {
        let n = 32_768;
        let tone_hz = 5.0_f32;
        let amp = 1.0_f32;
        let noise_amp = 0.02_f32; // PSD floor way below the tone bin
        // Cheap LCG for deterministic "noise" without adding a dep.
        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut rng = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (((seed >> 33) as u32 as f32) / (1u32 << 30) as f32) - 1.0
        };
        let samples: Vec<Complex32> = (0..n)
            .map(|k| {
                let t = k as f32 / WSPR_RATE_HZ;
                let phi = 2.0 * std::f32::consts::PI * tone_hz * t;
                Complex32 {
                    re: amp * phi.cos() + noise_amp * rng(),
                    im: amp * phi.sin() + noise_amp * rng(),
                }
            })
            .collect();
        let decodes = decode_slot(&samples);
        assert_eq!(decodes.len(), 1);
        let d = &decodes[0];
        assert!((d.freq_hz - tone_hz).abs() < 0.5, "freq={}", d.freq_hz);
        assert!(d.snr_db > 20.0, "snr={}", d.snr_db);
    }

    #[test]
    fn returns_empty_for_pure_noise() {
        let n = 32_768;
        let mut seed: u64 = 1;
        let mut rng = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (((seed >> 33) as u32 as f32) / (1u32 << 30) as f32) - 1.0
        };
        let samples: Vec<Complex32> = (0..n)
            .map(|_| Complex32 {
                re: rng(),
                im: rng(),
            })
            .collect();
        let decodes = decode_slot(&samples);
        assert!(
            decodes.is_empty(),
            "expected no detections on white noise, got {decodes:?}"
        );
    }
}
