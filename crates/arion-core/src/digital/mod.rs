//! Digital-mode pipeline.
//!
//! The DSP thread feeds demodulated audio (48 kHz real) into a
//! `DigitalPipeline` per RX when the user selects a digital decoder
//! (PSK31/RTTY/APRS). Pipeline order:
//!
//! 1. Resample 48 k → 12 k complex (imaginary part zeroed) via liquid.
//! 2. Mix the user-selected center frequency down to DC via liquid NCO.
//! 3. Low-pass filter, symbol-sync, BPSK differential demod **[TODO]**.
//! 4. Bit stream → `varicode::VaricodeDecoder` → ASCII text.
//!
//! Steps 1 and 2 are live; step 3 is stubbed so the varicode decoder
//! receives no bits yet. `VaricodeDecoder` itself is tested in
//! isolation and will produce the expected text once real bits flow.

pub mod varicode;

use liquid::{Complex32, MsResamp, Nco};

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

#[derive(Debug, Clone)]
pub struct DigitalDecode {
    pub mode: DigitalMode,
    pub text: String,
    pub snr_db: f32,
}

/// Per-RX digital decoder pipeline.
pub struct DigitalPipeline {
    mode: DigitalMode,
    resampler: MsResamp,
    nco: Nco,
    center_hz: f32,
    scratch_in: Vec<Complex32>,
    scratch_out: Vec<Complex32>,
    varicode: varicode::VaricodeDecoder,
    pending: Vec<DigitalDecode>,
}

const BASEBAND_RATE_HZ: f32 = 12_000.0;
const DEFAULT_PSK_CENTER_HZ: f32 = 1_500.0;

impl DigitalPipeline {
    pub fn new(mode: DigitalMode, input_rate_hz: u32) -> Option<Self> {
        let rate = BASEBAND_RATE_HZ / input_rate_hz as f32;
        let resampler = MsResamp::new(rate, 60.0).ok()?;
        let mut nco = Nco::new().ok()?;
        nco.set_frequency_hz(DEFAULT_PSK_CENTER_HZ, BASEBAND_RATE_HZ);
        Some(Self {
            mode,
            resampler,
            nco,
            center_hz: DEFAULT_PSK_CENTER_HZ,
            scratch_in: Vec::with_capacity(2048),
            scratch_out: Vec::with_capacity(2048),
            varicode: varicode::VaricodeDecoder::new(),
            pending: Vec::new(),
        })
    }

    pub fn mode(&self) -> DigitalMode {
        self.mode
    }

    pub fn center_hz(&self) -> f32 {
        self.center_hz
    }

    pub fn set_center_hz(&mut self, hz: f32) {
        self.center_hz = hz;
        self.nco.set_frequency_hz(hz, BASEBAND_RATE_HZ);
    }

    /// Push a block of post-AGC real audio. Steps 1–2 run live; step 3
    /// (demod) is TODO so no decodes are produced yet.
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
        let n = self
            .resampler
            .execute(&self.scratch_in, &mut self.scratch_out);

        // Carrier mix-down: a tone at center_hz becomes DC.
        for s in &mut self.scratch_out[..n] {
            *s = self.nco.mix_down_step(*s);
        }

        // TODO(F.1.2b): matched filter @ 31.25 baud, symbol sync
        // (Gardner or liquid symsync), differential BPSK hard decision
        // → bit stream → self.varicode.push_bit(...).
        let _ = &mut self.varicode;
    }

    pub fn drain_decodes(&mut self) -> Vec<DigitalDecode> {
        // When real demod lands, accumulate varicode output here:
        //   let text = self.varicode.drain();
        //   if !text.is_empty() { self.pending.push(DigitalDecode { ... }); }
        std::mem::take(&mut self.pending)
    }
}
