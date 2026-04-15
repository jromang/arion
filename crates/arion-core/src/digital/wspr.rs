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

/// WSPR parameters. Symbol rate equals tone spacing (Mueller-Müller
/// property), both exactly `WSPR_RATE_HZ / 256`.
const N_SYMBOLS: usize = wsprd::N_SYMBOLS;
const SAMPLES_PER_SYMBOL: usize = 256;
const TONE_SPACING_HZ: f32 = WSPR_RATE_HZ / SAMPLES_PER_SYMBOL as f32;

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
/// Pipeline:
/// 1. Spectral peak detection to find the carrier frequency.
/// 2. 4-FSK non-coherent energy demod at 162 symbol positions.
/// 3. Extract the data bit (top bit of each hard 4-FSK decision),
///    deinterleave, run the Fano K=32 r=1/2 decoder, and unpack
///    the 50-bit payload into `"CALLSIGN LOCATOR POWER"`.
///
/// A decode is only returned when the Fano decoder and `unpk_()`
/// both succeed. For weak / noisy signals (normal off-air SNRs
/// around –28 dB) a small freq/time sweep would help; today we
/// only try the peak-detected frequency with symbol timing at
/// the buffer origin, which is sufficient for self-generated
/// round-trip signals.
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

    // --- Stage 2: 4-FSK non-coherent demod over 162 symbols ---
    if samples.len() < N_SYMBOLS * SAMPLES_PER_SYMBOL {
        return Vec::new();
    }
    let mut hard = [0u8; N_SYMBOLS];
    for (k, out) in hard.iter_mut().enumerate() {
        let start = k * SAMPLES_PER_SYMBOL;
        let end = start + SAMPLES_PER_SYMBOL;
        let seg = &samples[start..end];
        let mut energies = [0.0_f32; 4];
        for (t, energy) in energies.iter_mut().enumerate() {
            // Tone t lives at freq_hz + (t - 1.5) · spacing so the
            // 4 tones straddle the detected peak symmetrically.
            let tone_hz = freq_hz + (t as f32 - 1.5) * TONE_SPACING_HZ;
            let dphi = 2.0 * std::f32::consts::PI * tone_hz / WSPR_RATE_HZ;
            let (mut re, mut im) = (0.0_f32, 0.0_f32);
            let mut phi = 0.0_f32;
            for s in seg {
                let c = phi.cos();
                let si = phi.sin();
                re += s.re * c + s.im * si;
                im += s.im * c - s.re * si;
                phi += dphi;
                if phi > std::f32::consts::TAU {
                    phi -= std::f32::consts::TAU;
                }
            }
            *energy = re * re + im * im;
        }
        let (best, _) = energies
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        *out = best as u8;
    }

    // --- Stage 3: data bits → Fano → unpack ---
    // Channel symbol t = 2 · data_bit + sync_bit, so data bit = t >> 1.
    let mut soft = [0u8; N_SYMBOLS];
    for (i, &t) in hard.iter().enumerate() {
        soft[i] = if t >> 1 == 1 { 255 } else { 0 };
    }
    wsprd::deinterleave(&mut soft);
    let Ok(decdata) = wsprd::fano_decode(&mut soft) else {
        return Vec::new();
    };
    let Ok(text) = wsprd::unpack(&decdata) else {
        return Vec::new();
    };

    let mut parts = text.split_whitespace();
    let callsign = parts.next().unwrap_or("?").to_string();
    let locator = parts.next().unwrap_or("").to_string();
    let power_dbm = parts
        .next()
        .and_then(|s| s.parse::<i8>().ok())
        .unwrap_or(0);

    vec![WsprDecode {
        callsign,
        locator,
        power_dbm,
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

/// Generate a synthetic WSPR baseband signal at 375 Hz complex for
/// the given message text. Exposed as `#[cfg(test)]` — the test
/// path is the only caller.
#[cfg(test)]
fn synth_baseband(message: &str, freq_hz: f32) -> Vec<Complex32> {
    let symbols = wsprd::channel_symbols(message).unwrap();
    let mut out = Vec::with_capacity(N_SYMBOLS * SAMPLES_PER_SYMBOL);
    let mut phase = 0.0_f32;
    for &t in &symbols {
        let tone_hz = freq_hz + (t as f32 - 1.5) * TONE_SPACING_HZ;
        let dphi = 2.0 * std::f32::consts::PI * tone_hz / WSPR_RATE_HZ;
        for _ in 0..SAMPLES_PER_SYMBOL {
            out.push(Complex32 {
                re: phase.cos(),
                im: phase.sin(),
            });
            phase += dphi;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_synthetic_wspr_signal() {
        // Encode "AA0AA EM15 37" into a clean 375 Hz baseband
        // signal at 50 Hz offset, feed it through decode_slot,
        // expect the full message back.
        let samples = synth_baseband("AA0AA EM15 37", 50.0);
        let decodes = decode_slot(&samples);
        assert_eq!(decodes.len(), 1);
        assert_eq!(decodes[0].callsign, "AA0AA");
        assert_eq!(decodes[0].locator, "EM15");
        assert_eq!(decodes[0].power_dbm, 37);
        // Detected frequency should land near the injected tone
        // (within one 375/32768 ≈ 0.011 Hz bin, plus FFT binning
        // for the "center" of the 4-tone group).
        assert!(
            (decodes[0].freq_hz - 50.0).abs() < 3.0,
            "freq_hz = {}",
            decodes[0].freq_hz
        );
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
