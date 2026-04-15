//! PSK31 modulator + demodulator.
//!
//! **Modulator** (`encode_text`): text → varicode bits → differential
//! BPSK symbols → raised-cosine-enveloped carrier at 48 kHz real.
//!
//! **Demodulator** (`Psk31Demod`): carrier mix-down → matched filter
//! → symbol decimation → differential phase detection → bit stream
//! → varicode decoder.
//!
//! Symbol rate 31.25 Hz. Carrier 1500 Hz by default. The modulator and
//! demodulator share the same carrier/sample-rate conventions so a
//! self-generated signal round-trips cleanly; real off-air signals
//! will need symbol timing recovery (Gardner or liquid symsync) on
//! top of this demod — flagged as TODO inside `process_block`.
//!
//! Canonical reference: G3PLX, "PSK31 Fundamentals" (1998).

use super::varicode::{self, VaricodeDecoder};

pub const SAMPLE_RATE: f32 = 48_000.0;
pub const BAUD: f32 = 31.25;
pub const SAMPLES_PER_SYMBOL: usize = (SAMPLE_RATE / BAUD) as usize; // 1536
pub const DEFAULT_CARRIER_HZ: f32 = 1_500.0;

/// Half-cosine envelope over one symbol. Amplitude is 1 at the center
/// of the symbol and 0 at the edges, so consecutive symbols that have
/// the same differential bit sum to a flat carrier and consecutive
/// opposite-bit symbols pass through zero (the canonical PSK31 "nulls
/// at phase reversal" that give the mode its narrow spectrum).
fn envelope(t_frac: f32) -> f32 {
    // t_frac in [0, 1] over one symbol period.
    (std::f32::consts::PI * (t_frac - 0.5)).cos()
}

/// Encode text as a 48 kHz real PSK31 audio stream.
///
/// Prefixes and trails a short idle (all-ones = flat carrier) so the
/// decoder has carrier to lock onto.
pub fn encode_text(text: &str, carrier_hz: f32) -> Vec<f32> {
    let mut bits: Vec<bool> = Vec::with_capacity(text.len() * 16);
    // Lead-in idle: PSK31 convention is idle = stream of zero bits
    // (continuous phase reversals every symbol). In varicode the "00"
    // separator resets the accumulator, so idle zeros produce no
    // character and cleanly frame the first real code that follows.
    // 33 = 32 + 1 sacrificial bit consumed by the demod's priming
    // step, so the varicode stream heading into the first real
    // character starts on a clean "00" boundary.
    bits.extend(std::iter::repeat_n(false, 33));
    for c in text.bytes() {
        if let Some(code) = varicode::code_for(c) {
            for b in code.bytes() {
                bits.push(b == b'1');
            }
            bits.push(false);
            bits.push(false);
        }
    }
    // 33 = 32 + 1 sacrificial bit consumed by the demod's priming
    // step, so the varicode stream heading into the first real
    // character starts on a clean "00" boundary.
    bits.extend(std::iter::repeat_n(false, 33));

    // Differential BPSK: symbol[k] = symbol[k-1] * (bit==1 ? +1 : -1).
    // PSK31 convention: "1" = no phase change, "0" = 180° phase change.
    let mut symbols: Vec<i8> = Vec::with_capacity(bits.len());
    let mut cur: i8 = 1;
    for &b in &bits {
        if !b {
            cur = -cur;
        }
        symbols.push(cur);
    }

    let n = symbols.len() * SAMPLES_PER_SYMBOL;
    let mut out = Vec::with_capacity(n);
    let two_pi_fc_over_fs = 2.0 * std::f32::consts::PI * carrier_hz / SAMPLE_RATE;
    for (k, &d) in symbols.iter().enumerate() {
        for i in 0..SAMPLES_PER_SYMBOL {
            let sample_idx = (k * SAMPLES_PER_SYMBOL + i) as f32;
            let t_frac = i as f32 / SAMPLES_PER_SYMBOL as f32;
            let amp = envelope(t_frac) * d as f32;
            out.push(amp * (two_pi_fc_over_fs * sample_idx).cos());
        }
    }
    out
}

/// PSK31 demodulator. Operates on 48 kHz real audio (same rate as the
/// DSP tap feeding `DigitalPipeline`).
pub struct Psk31Demod {
    carrier_hz: f32,
    /// NCO accumulator (radians).
    phase: f32,
    /// Running matched-filter accumulator over one symbol.
    acc_i: f32,
    acc_q: f32,
    /// Sample counter within the current symbol window [0,
    /// SAMPLES_PER_SYMBOL).
    symbol_phase: usize,
    /// Previous symbol's complex value (for differential demod).
    prev: (f32, f32),
    /// Seen at least one complete symbol yet?
    primed: bool,
    varicode: VaricodeDecoder,
    pending_text: String,
}

impl Psk31Demod {
    pub fn new(carrier_hz: f32) -> Self {
        Self {
            carrier_hz,
            phase: 0.0,
            acc_i: 0.0,
            acc_q: 0.0,
            symbol_phase: 0,
            prev: (1.0, 0.0),
            primed: false,
            varicode: VaricodeDecoder::new(),
            pending_text: String::new(),
        }
    }

    /// Feed audio samples at 48 kHz. Any newly-decoded characters are
    /// appended to an internal buffer — drain with `drain_text`.
    pub fn process_block(&mut self, audio: &[f32]) {
        let dtheta = 2.0 * std::f32::consts::PI * self.carrier_hz / SAMPLE_RATE;
        for &x in audio {
            // Mix down with cos + sin NCO to get complex baseband.
            let (s, c) = (self.phase.sin(), self.phase.cos());
            self.acc_i += x * c;
            self.acc_q += x * -s;

            self.phase += dtheta;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }

            self.symbol_phase += 1;
            if self.symbol_phase >= SAMPLES_PER_SYMBOL {
                // Symbol boundary: matched filter output is (acc_i,
                // acc_q) summed over exactly one symbol. For a BPSK
                // signal this is ±magnitude along the carrier axis.
                let cur = (self.acc_i, self.acc_q);
                if self.primed {
                    // Differential demod: bit = sign(<cur, prev>).
                    // Positive dot product → same phase → bit 1.
                    let dot = cur.0 * self.prev.0 + cur.1 * self.prev.1;
                    let bit = dot > 0.0;
                    self.varicode.push_bit(bit);
                    let decoded = self.varicode.drain();
                    if !decoded.is_empty() {
                        self.pending_text.push_str(&decoded);
                    }
                }
                self.prev = cur;
                self.primed = true;
                self.acc_i = 0.0;
                self.acc_q = 0.0;
                self.symbol_phase = 0;
                // TODO(F.1.2d): Gardner or liquid symsync timing
                // correction around here to lock onto signals whose
                // symbol phase doesn't match the arbitrary block
                // boundary we chose at init.
            }
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
    fn round_trip_hi() {
        let audio = encode_text("hi", DEFAULT_CARRIER_HZ);
        let mut d = Psk31Demod::new(DEFAULT_CARRIER_HZ);
        d.process_block(&audio);
        let out = d.drain_text();
        assert!(out.contains("hi"), "decoded text: {out:?}");
    }

    #[test]
    fn round_trip_abcdef() {
        let audio = encode_text("abcdef", DEFAULT_CARRIER_HZ);
        let mut d = Psk31Demod::new(DEFAULT_CARRIER_HZ);
        d.process_block(&audio);
        let out = d.drain_text();
        assert!(out.contains("abcdef"), "decoded text: {out:?}");
    }
}
