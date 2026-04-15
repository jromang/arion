//! RTTY modulator + demodulator.
//!
//! Standard amateur parameters: 45.45 baud, 170 Hz shift, ITA2 Baudot
//! 5N1.5 (1 start bit = space, 5 data bits LSB first, 1.5 stop bits =
//! mark). Mark tone = higher frequency.
//!
//! The demod uses two NCO-driven tone detectors: each sample is mixed
//! down by both the mark and the space carriers and integrated over
//! roughly one bit period. The sign of (|mark|² − |space|²) at bit
//! center is the hard decision.
//!
//! Bit timing is a classic start-bit edge-trigger state machine:
//! detect a mark→space transition, wait half a bit, then sample 5 bits
//! at one-bit intervals. Sufficient for self-generated round-trip
//! tests; off-air signals with clock drift will want liquid symsync
//! on the discriminator output (TODO).

use super::baudot::{self, Shift};

pub const SAMPLE_RATE: f32 = 48_000.0;
pub const BAUD: f32 = 45.45;
pub const DEFAULT_MARK_HZ: f32 = 1_445.0;
pub const DEFAULT_SPACE_HZ: f32 = 1_275.0;
pub const SAMPLES_PER_BIT: f32 = SAMPLE_RATE / BAUD;

// Low-pass IIR α for the I/Q components *before* squaring. Bandwidth
// must be well below the mark↔space spacing (170 Hz) so the off-tone
// mixdown (which sits at ±170 Hz) is rejected, while still fast
// enough to track one-bit transitions (~22 ms at 45.45 bd).
// α ≈ 2π·f_cut / f_s, here f_cut ≈ 40 Hz.
const IQ_ALPHA: f32 = 0.005;

/// Encode text as a 48 kHz real RTTY audio stream.
pub fn encode_text(text: &str, mark_hz: f32, space_hz: f32) -> Vec<f32> {
    let mut bits: Vec<bool> = Vec::new();
    // Lead-in idle: ~500 ms of mark (line idle state).
    let idle_bits = (BAUD * 0.5) as usize;
    bits.extend(std::iter::repeat_n(true, idle_bits));

    let mut shift = Shift::Letters;
    for c in text.bytes() {
        if let Some((code, needed)) = baudot::encode(c) {
            if needed != shift {
                let shift_code = match needed {
                    Shift::Letters => baudot::CODE_LTRS_SHIFT,
                    Shift::Figures => baudot::CODE_FIGS_SHIFT,
                };
                emit_char(&mut bits, shift_code);
                shift = needed;
            }
            emit_char(&mut bits, code);
        }
    }
    bits.extend(std::iter::repeat_n(true, idle_bits));

    // Convert bits to a 48 kHz audio stream. Bit duration is
    // SAMPLES_PER_BIT (non-integer — accumulate fractional sample
    // index so we don't accrue drift over a long message).
    let mut out = Vec::with_capacity((bits.len() as f32 * SAMPLES_PER_BIT) as usize);
    let mut sample_idx = 0.0_f32;
    let mut phase = 0.0_f32;
    for (k, &b) in bits.iter().enumerate() {
        let freq = if b { mark_hz } else { space_hz };
        let target = (k + 1) as f32 * SAMPLES_PER_BIT;
        let dphi = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE;
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

/// Emit one Baudot character to the bit stream: start bit (space=0),
/// 5 data bits LSB first, 1.5 stop bits (we emit 2 stop bits which is
/// always acceptable to decoders).
fn emit_char(bits: &mut Vec<bool>, code: u8) {
    bits.push(false);
    for i in 0..5 {
        bits.push(((code >> i) & 1) != 0);
    }
    bits.push(true);
    bits.push(true);
}

pub struct RttyDemod {
    mark_hz: f32,
    space_hz: f32,
    mark_phase: f32,
    space_phase: f32,
    /// IIR-low-passed complex components of each tone's baseband
    /// mix-down. Squared magnitude of these is the actual tone
    /// energy (squaring before filtering gives the same number for
    /// both tones and defeats discrimination).
    mark_i: f32,
    mark_q: f32,
    space_i: f32,
    space_q: f32,
    /// Last discriminator sign (mark > space ? 1 : 0).
    last_mark: bool,
    /// Bit state machine.
    state: State,
    /// Fractional sample counter within the current bit period.
    bit_pos: f32,
    /// Accumulated data bits for the current character.
    data_code: u8,
    data_count: u8,
    shift: Shift,
    pending_text: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum State {
    /// Waiting for a mark→space edge (start bit).
    Idle,
    /// Inside the first half of the start bit — validate at its
    /// center that we're still seeing space.
    StartBit,
    /// Sampling the 5 data bits at one-bit intervals starting from
    /// the center of bit 0.
    Data,
    /// Consuming the stop bits; ignore content.
    Stop,
}

impl RttyDemod {
    pub fn new(mark_hz: f32, space_hz: f32) -> Self {
        Self {
            mark_hz,
            space_hz,
            mark_phase: 0.0,
            space_phase: 0.0,
            // Seed so the initial tone comparison favors mark
            // (idle state) — else the very first sample sees
            // both magnitudes at zero and spuriously flips.
            mark_i: 1.0,
            mark_q: 0.0,
            space_i: 0.0,
            space_q: 0.0,
            last_mark: true,
            state: State::Idle,
            bit_pos: 0.0,
            data_code: 0,
            data_count: 0,
            shift: Shift::Letters,
            pending_text: String::new(),
        }
    }

    pub fn process_block(&mut self, audio: &[f32]) {
        let dth_m = 2.0 * std::f32::consts::PI * self.mark_hz / SAMPLE_RATE;
        let dth_s = 2.0 * std::f32::consts::PI * self.space_hz / SAMPLE_RATE;

        for &x in audio {
            // Mix down both tones.
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

            // Low-pass the I and Q components of each mix-down so the
            // off-tone sinusoid at ±170 Hz is rejected, then compare
            // squared magnitudes.
            self.mark_i += IQ_ALPHA * (mi - self.mark_i);
            self.mark_q += IQ_ALPHA * (mq - self.mark_q);
            self.space_i += IQ_ALPHA * (si - self.space_i);
            self.space_q += IQ_ALPHA * (sq - self.space_q);
            let m2 = self.mark_i * self.mark_i + self.mark_q * self.mark_q;
            let s2 = self.space_i * self.space_i + self.space_q * self.space_q;

            let is_mark = m2 > s2;

            match self.state {
                State::Idle => {
                    if self.last_mark && !is_mark {
                        self.state = State::StartBit;
                        self.bit_pos = 0.0;
                    }
                }
                State::StartBit => {
                    self.bit_pos += 1.0;
                    if self.bit_pos >= SAMPLES_PER_BIT * 0.5 {
                        // At the center of the start bit: confirm space.
                        if !is_mark {
                            self.state = State::Data;
                            self.bit_pos = 0.0;
                            self.data_code = 0;
                            self.data_count = 0;
                        } else {
                            // Glitch — go back to idle.
                            self.state = State::Idle;
                        }
                    }
                }
                State::Data => {
                    self.bit_pos += 1.0;
                    if self.bit_pos >= SAMPLES_PER_BIT {
                        // Center of the next data bit.
                        if is_mark {
                            self.data_code |= 1 << self.data_count;
                        }
                        self.data_count += 1;
                        self.bit_pos = 0.0;
                        if self.data_count == 5 {
                            self.state = State::Stop;
                        }
                    }
                }
                State::Stop => {
                    self.bit_pos += 1.0;
                    if self.bit_pos >= SAMPLES_PER_BIT {
                        // End of the 5 data bits: emit the character,
                        // then wait for the next start-bit edge.
                        match self.data_code {
                            baudot::CODE_LTRS_SHIFT => self.shift = Shift::Letters,
                            baudot::CODE_FIGS_SHIFT => self.shift = Shift::Figures,
                            code => {
                                if let Some(ch) = baudot::decode(code, self.shift) {
                                    self.pending_text.push(ch as char);
                                }
                            }
                        }
                        self.state = State::Idle;
                    }
                }
            }

            self.last_mark = is_mark;
        }
    }

    pub fn drain_text(&mut self) -> String {
        std::mem::take(&mut self.pending_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_hello() {
        let audio = encode_text("HELLO", DEFAULT_MARK_HZ, DEFAULT_SPACE_HZ);
        let mut d = RttyDemod::new(DEFAULT_MARK_HZ, DEFAULT_SPACE_HZ);
        d.process_block(&audio);
        let out = d.drain_text();
        assert!(out.contains("HELLO"), "decoded: {out:?}");
    }

    #[test]
    fn round_trip_mixed_case_and_digits() {
        let audio = encode_text("CQ DE F4XYZ 59", DEFAULT_MARK_HZ, DEFAULT_SPACE_HZ);
        let mut d = RttyDemod::new(DEFAULT_MARK_HZ, DEFAULT_SPACE_HZ);
        d.process_block(&audio);
        let out = d.drain_text();
        assert!(out.contains("CQ DE F4XYZ 59"), "decoded: {out:?}");
    }
}
