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

use liquid::{Complex32, MsResamp, SymSync};

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
    // Lead-in idle: zero bits = continuous phase reversals. Long
    // enough to absorb the demod pipeline delay (NCO phase lock +
    // MsResamp transient + SymSync filter bank warm-up + one
    // priming symbol) and leave the varicode decoder on a clean
    // "00" boundary before the first real character.
    bits.extend(std::iter::repeat_n(false, 256));
    for c in text.bytes() {
        if let Some(code) = varicode::code_for(c) {
            for b in code.bytes() {
                bits.push(b == b'1');
            }
            bits.push(false);
            bits.push(false);
        }
    }
    // Lead-in idle: zero bits = continuous phase reversals. Long
    // enough to absorb the demod pipeline delay (NCO phase lock +
    // MsResamp transient + SymSync filter bank warm-up + one
    // priming symbol) and leave the varicode decoder on a clean
    // "00" boundary before the first real character.
    bits.extend(std::iter::repeat_n(false, 128));

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

// Baseband pipeline:
//   48 kHz real in → NCO mix-down → 48 kHz complex baseband
//   → MsResamp → ~250 Hz complex (8 samples per 31.25 baud symbol)
//   → SymSync (Kaiser polyphase) → 1 sample per symbol
//   → differential BPSK → bit → varicode
//
// Decimating first makes the symsync filter bank tractable
// (internal state proportional to k·m·M; k=8 keeps it tiny).
const SYMSYNC_K: u32 = 8;
const SYMSYNC_M: u32 = 3;
const SYMSYNC_BETA: f32 = 0.3;
const SYMSYNC_BANK: u32 = 32;
const BASEBAND_RATE: f32 = SYMSYNC_K as f32 * BAUD; // 250 Hz

/// PSK31 demodulator. Operates on 48 kHz real audio (same rate as the
/// DSP tap feeding `DigitalPipeline`).
pub struct Psk31Demod {
    carrier_hz: f32,
    /// NCO accumulator (radians).
    phase: f32,
    /// 48 kHz → BASEBAND_RATE complex decimator.
    resampler: MsResamp,
    /// Kaiser polyphase symbol synchronizer (BASEBAND_RATE → 1 sa/sym).
    symsync: SymSync,
    /// Scratch buffers.
    baseband: Vec<Complex32>,
    decimated: Vec<Complex32>,
    symbols: Vec<Complex32>,
    /// Differential demod state: previous symbol.
    prev: Complex32,
    primed: bool,
    varicode: VaricodeDecoder,
    pending_text: String,
}

impl Psk31Demod {
    pub fn new(carrier_hz: f32) -> Self {
        let resampler = MsResamp::new(BASEBAND_RATE / SAMPLE_RATE, 60.0)
            .expect("msresamp_crcf_create");
        let mut symsync = SymSync::new_kaiser(SYMSYNC_K, SYMSYNC_M, SYMSYNC_BETA, SYMSYNC_BANK)
            .expect("symsync_crcf_create_kaiser");
        // Moderate loop bandwidth: fast enough to lock within the
        // encoder's 128-bit idle preamble, narrow enough to track
        // through a real HF channel without chasing noise.
        symsync.set_loop_bandwidth(0.05);
        Self {
            carrier_hz,
            phase: 0.0,
            resampler,
            symsync,
            baseband: Vec::with_capacity(2048),
            decimated: Vec::with_capacity(64),
            symbols: Vec::with_capacity(16),
            prev: Complex32 { re: 1.0, im: 0.0 },
            primed: false,
            varicode: VaricodeDecoder::new(),
            pending_text: String::new(),
        }
    }

    /// Feed audio samples at 48 kHz. Any newly-decoded characters are
    /// appended to an internal buffer — drain with `drain_text`.
    pub fn process_block(&mut self, audio: &[f32]) {
        // Stage 1: NCO mix-down to complex baseband.
        self.baseband.clear();
        self.baseband.reserve(audio.len());
        let dtheta = 2.0 * std::f32::consts::PI * self.carrier_hz / SAMPLE_RATE;
        for &x in audio {
            let (s, c) = (self.phase.sin(), self.phase.cos());
            self.baseband.push(Complex32 {
                re: x * c,
                im: -x * s,
            });
            self.phase += dtheta;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
        }

        // Stage 2: decimate 48 k → ~250 Hz.
        let cap = self.resampler.num_output(audio.len() as u32) as usize + 16;
        if self.decimated.len() < cap {
            self.decimated.resize(cap, Complex32 { re: 0.0, im: 0.0 });
        }
        let n_dec = self.resampler.execute(&self.baseband, &mut self.decimated);

        // Stage 3: symbol sync → one complex sample per symbol.
        let sym_cap = n_dec / SYMSYNC_K as usize + 4;
        if self.symbols.len() < sym_cap {
            self.symbols.resize(sym_cap, Complex32 { re: 0.0, im: 0.0 });
        }
        let n_sym = self.symsync.execute(&self.decimated[..n_dec], &mut self.symbols);

        // Stage 4: differential BPSK → bit → varicode.
        for &cur in &self.symbols[..n_sym] {
            if self.primed {
                let dot = cur.re * self.prev.re + cur.im * self.prev.im;
                self.varicode.push_bit(dot > 0.0);
                let decoded = self.varicode.drain();
                if !decoded.is_empty() {
                    self.pending_text.push_str(&decoded);
                }
            }
            self.prev = cur;
            self.primed = true;
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

    /// Simulate an off-air signal starting mid-symbol — the symsync
    /// loop must re-lock timing and recover the message. The first
    /// character may be lost or garbled during lock acquisition
    /// (matches fldigi behaviour; real PSK31 operators always expect
    /// some preamble text to be corrupted before carrier+symbol locks
    /// settle). Check that a substring of the trailing message
    /// decodes cleanly.
    #[test]
    fn round_trip_with_timing_offset() {
        let mut audio = encode_text("garbagexxxhello", DEFAULT_CARRIER_HZ);
        audio.drain(..737); // ≈ 0.48 of a 1536-sample symbol
        let mut d = Psk31Demod::new(DEFAULT_CARRIER_HZ);
        d.process_block(&audio);
        let out = d.drain_text();
        assert!(out.contains("hello"), "decoded text: {out:?}");
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
