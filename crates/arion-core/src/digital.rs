//! Digital-mode pipeline.
//!
//! The DSP thread feeds demodulated audio (48 kHz real) into a
//! `DigitalPipeline` per RX when the user selects a digital decoder
//! (PSK31/RTTY/APRS). The pipeline resamples to 12 kHz complex
//! (mandatory for most ham digital modes) and runs the demod.
//!
//! The resampler is always live once a mode is selected; the actual
//! demod is stubbed and returns no decodes today. PSK31 demod is the
//! next increment: NCO carrier recovery, symsync + `liquid::Modem`
//! for BPSK, then varicode → text.

use liquid::{Complex32, MsResamp};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DigitalMode {
    Psk31,
    Psk63,
    Rtty,
    Aprs,
}

impl DigitalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Psk31 => "psk31",
            Self::Psk63 => "psk63",
            Self::Rtty => "rtty",
            Self::Aprs => "aprs",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "psk31" => Some(Self::Psk31),
            "psk63" => Some(Self::Psk63),
            "rtty" => Some(Self::Rtty),
            "aprs" => Some(Self::Aprs),
            _ => None,
        }
    }
}

/// A decoded unit of information from a digital mode pipeline.
#[derive(Debug, Clone)]
pub struct DigitalDecode {
    pub mode: DigitalMode,
    pub text: String,
    pub snr_db: f32,
}

/// Per-RX digital decoder pipeline.
///
/// Audio in: post-AGC real samples at 48 kHz (one per call). Internally
/// resampled to 12 kHz complex (Hilbert-style analytic via zeroing the
/// imaginary component is a placeholder; real carrier mixing arrives
/// with PSK31 demod).
pub struct DigitalPipeline {
    mode: DigitalMode,
    resampler: MsResamp,
    /// Scratch complex buffer for resampler input. Grows as needed.
    scratch_in: Vec<Complex32>,
    /// Scratch output at 12 kHz.
    scratch_out: Vec<Complex32>,
    /// Pending decodes awaiting the next telemetry publish.
    pending: Vec<DigitalDecode>,
}

impl DigitalPipeline {
    pub fn new(mode: DigitalMode, input_rate_hz: u32) -> Option<Self> {
        let target = 12_000.0_f32;
        let rate = target / input_rate_hz as f32;
        let resampler = MsResamp::new(rate, 60.0).ok()?;
        Some(Self {
            mode,
            resampler,
            scratch_in: Vec::with_capacity(2048),
            scratch_out: Vec::with_capacity(2048),
            pending: Vec::new(),
        })
    }

    pub fn mode(&self) -> DigitalMode {
        self.mode
    }

    /// Push a block of post-AGC real-valued audio (48 kHz). Advances
    /// the pipeline. Any decodes produced are accumulated and drained
    /// via `drain_decodes`.
    pub fn push_audio(&mut self, audio: &[f32]) {
        self.scratch_in.clear();
        self.scratch_in.reserve(audio.len());
        for &s in audio {
            self.scratch_in.push(Complex32 { re: s, im: 0.0 });
        }
        let cap = self.resampler.num_output(audio.len() as u32) as usize + 16;
        if self.scratch_out.len() < cap {
            self.scratch_out.resize(cap, Complex32 { re: 0.0, im: 0.0 });
        }
        let _n = self
            .resampler
            .execute(&self.scratch_in, &mut self.scratch_out);
        // Demod + varicode/Baudot/AX.25 decoding arrives in the next
        // increment. Today the pipeline is audible-plumbing only:
        // resampled samples are discarded after the call.
        let _ = self.mode;
    }

    pub fn drain_decodes(&mut self) -> Vec<DigitalDecode> {
        std::mem::take(&mut self.pending)
    }
}
