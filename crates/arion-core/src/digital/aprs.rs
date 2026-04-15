//! APRS: AFSK 1200 Bell 202 + HDLC + NRZI + AX.25.
//!
//! Audio carriers: **mark = 1200 Hz**, **space = 2200 Hz**. Symbol
//! rate: **1200 baud**. Transmitted bit encoding is NRZI (0 = tone
//! transition, 1 = no transition), wrapped by HDLC bit-stuffing and
//! 0x7E flag delimiters around an AX.25 UI frame.
//!
//! Round-trip test exercises `encode_frame → AprsDemod` so the
//! Bell-202 modem, NRZI state machine, HDLC framing, and AX.25
//! parser all agree on wire layout.

use super::ax25::{self, UiFrame};

pub const SAMPLE_RATE: f32 = 48_000.0;
pub const BAUD: f32 = 1_200.0;
pub const MARK_HZ: f32 = 1_200.0;
pub const SPACE_HZ: f32 = 2_200.0;
pub const SAMPLES_PER_BIT: f32 = SAMPLE_RATE / BAUD; // 40.0

// I/Q low-pass: bandwidth much less than the 1000 Hz mark↔space
// spacing, tight enough to reject the off-tone sinusoid but fast
// enough to track 1200 bd transitions (≈ 830 µs per bit).
const IQ_ALPHA: f32 = 0.08;

/// Encode an AX.25 UI frame as a 48 kHz real AFSK audio stream.
pub fn encode_frame(frame: &UiFrame) -> Vec<f32> {
    let bits = ax25::frame_bits(frame, 8, 4);
    // NRZI: encode each bit as transition (0) or no transition (1).
    // Transmitted tone toggles on a `false` bit.
    let mut tx_bits: Vec<bool> = Vec::with_capacity(bits.len());
    let mut mark = true;
    for &b in &bits {
        if !b {
            mark = !mark;
        }
        tx_bits.push(mark);
    }

    let mut out = Vec::with_capacity((tx_bits.len() as f32 * SAMPLES_PER_BIT) as usize);
    let mut phase = 0.0_f32;
    let mut sample_idx = 0.0_f32;
    for (k, &is_mark) in tx_bits.iter().enumerate() {
        let freq = if is_mark { MARK_HZ } else { SPACE_HZ };
        let dphi = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE;
        let target = (k + 1) as f32 * SAMPLES_PER_BIT;
        while sample_idx < target {
            out.push(phase.cos());
            phase += dphi;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
            sample_idx += 1.0;
        }
    }
    out
}

pub struct AprsDemod {
    mark_phase: f32,
    space_phase: f32,
    mark_i: f32,
    mark_q: f32,
    space_i: f32,
    space_q: f32,
    /// Current instantaneous tone state (updated each sample).
    is_mark: bool,
    /// Tone state at the previous bit-clock tick (for NRZI decode).
    last_sampled_mark: bool,
    /// Fractional bit clock; ticks once when ≥ SAMPLES_PER_BIT.
    bit_clock: f32,
    /// Decoded (post-NRZI) bit stream since the last reset.
    bits: Vec<bool>,
    /// Max bits to keep — trimmed periodically.
    max_bits: usize,
    pending: Vec<UiFrame>,
}

impl AprsDemod {
    pub fn new() -> Self {
        Self {
            mark_phase: 0.0,
            space_phase: 0.0,
            mark_i: 1.0,
            mark_q: 0.0,
            space_i: 0.0,
            space_q: 0.0,
            is_mark: true,
            last_sampled_mark: true,
            bit_clock: 0.0,
            bits: Vec::with_capacity(4096),
            max_bits: 8192,
            pending: Vec::new(),
        }
    }

    pub fn process_block(&mut self, audio: &[f32]) {
        let dth_m = 2.0 * std::f32::consts::PI * MARK_HZ / SAMPLE_RATE;
        let dth_s = 2.0 * std::f32::consts::PI * SPACE_HZ / SAMPLE_RATE;

        for &x in audio {
            let mi = x * self.mark_phase.cos();
            let mq = -x * self.mark_phase.sin();
            let si = x * self.space_phase.cos();
            let sq = -x * self.space_phase.sin();
            self.mark_phase += dth_m;
            self.space_phase += dth_s;
            if self.mark_phase > std::f32::consts::TAU {
                self.mark_phase -= std::f32::consts::TAU;
            }
            if self.space_phase > std::f32::consts::TAU {
                self.space_phase -= std::f32::consts::TAU;
            }

            self.mark_i += IQ_ALPHA * (mi - self.mark_i);
            self.mark_q += IQ_ALPHA * (mq - self.mark_q);
            self.space_i += IQ_ALPHA * (si - self.space_i);
            self.space_q += IQ_ALPHA * (sq - self.space_q);
            let m2 = self.mark_i * self.mark_i + self.mark_q * self.mark_q;
            let s2 = self.space_i * self.space_i + self.space_q * self.space_q;
            let new_is_mark = m2 > s2;

            // DPLL-style clock recovery: on every tone edge, nudge
            // the bit clock toward the middle of the bit cell so we
            // always sample far from transitions (the IIR filter has
            // a few-sample lag relative to the true edge, so we also
            // don't want to sample *at* the edge).
            if new_is_mark != self.is_mark {
                // Pull the fractional clock toward SAMPLES_PER_BIT/2,
                // mixing gently to track small rate mismatches.
                let target = SAMPLES_PER_BIT * 0.5;
                self.bit_clock = 0.75 * self.bit_clock + 0.25 * target;
                self.is_mark = new_is_mark;
            }

            self.bit_clock += 1.0;
            let mut bit_emitted = false;
            if self.bit_clock >= SAMPLES_PER_BIT {
                // NRZI: emit 0 when the tone changed since the last
                // sample point, 1 otherwise.
                let nrzi_bit = self.is_mark == self.last_sampled_mark;
                self.bits.push(nrzi_bit);
                self.last_sampled_mark = self.is_mark;
                self.bit_clock -= SAMPLES_PER_BIT;
                bit_emitted = true;
            }

            if bit_emitted && self.bits.len() >= 8 * 20 {
                // Periodically scan for complete frames. Only trim
                // after a successful decode (or when the buffer is
                // full) so partial frames aren't cut in half.
                if let Some(frame) = ax25::scan_bits_for_ui(&self.bits) {
                    self.pending.push(frame);
                    self.bits.clear();
                } else if self.bits.len() > self.max_bits {
                    let keep = self.max_bits / 2;
                    let new_start = self.bits.len() - keep;
                    self.bits.drain(..new_start);
                }
            }
        }
    }

    pub fn drain(&mut self) -> Vec<UiFrame> {
        std::mem::take(&mut self.pending)
    }
}

impl Default for AprsDemod {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::ax25::Callsign;
    use super::*;

    fn sample_frame() -> UiFrame {
        UiFrame {
            dest: Callsign::new("APZ000", 0),
            src: Callsign::new("F4XYZ", 0),
            info: b">hello aprs".to_vec(),
        }
    }

    #[test]
    fn round_trip_ui_frame() {
        let audio = encode_frame(&sample_frame());
        let mut d = AprsDemod::new();
        for chunk in audio.chunks(1024) {
            d.process_block(chunk);
        }
        let frames = d.drain();
        assert!(!frames.is_empty(), "no frames decoded");
        assert_eq!(frames[0].header(), "F4XYZ>APZ000");
        assert_eq!(frames[0].info_str(), ">hello aprs");
    }
}
