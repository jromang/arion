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

pub mod baudot;
pub mod psk31;
pub mod rtty;
pub mod varicode;

use psk31::Psk31Demod;
use rtty::RttyDemod;

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
    center_hz: f32,
    psk31: Option<Psk31Demod>,
    rtty: Option<RttyDemod>,
    pending: Vec<DigitalDecode>,
}

const DEFAULT_PSK_CENTER_HZ: f32 = 1_500.0;

impl DigitalPipeline {
    pub fn new(mode: DigitalMode, _input_rate_hz: u32) -> Option<Self> {
        let psk31 = match mode {
            DigitalMode::Psk31 => Some(Psk31Demod::new(DEFAULT_PSK_CENTER_HZ)),
            _ => None,
        };
        let rtty = match mode {
            DigitalMode::Rtty => Some(RttyDemod::new(
                rtty::DEFAULT_MARK_HZ,
                rtty::DEFAULT_SPACE_HZ,
            )),
            _ => None,
        };
        Some(Self {
            mode,
            center_hz: DEFAULT_PSK_CENTER_HZ,
            psk31,
            rtty,
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
        if matches!(self.mode, DigitalMode::Psk31) {
            self.psk31 = Some(Psk31Demod::new(hz));
        }
    }

    /// Push a block of post-AGC real audio at 48 kHz. Decodes
    /// accumulate and drain via `drain_decodes`.
    pub fn push_audio(&mut self, audio: &[f32]) {
        let text = if let Some(demod) = self.psk31.as_mut() {
            demod.process_block(audio);
            demod.drain_text()
        } else if let Some(demod) = self.rtty.as_mut() {
            demod.process_block(audio);
            demod.drain_text()
        } else {
            String::new()
        };
        if !text.is_empty() {
            self.pending.push(DigitalDecode {
                mode: self.mode,
                text,
                snr_db: 0.0,
            });
        }
    }

    pub fn drain_decodes(&mut self) -> Vec<DigitalDecode> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_decodes_self_generated_rtty() {
        let audio = rtty::encode_text("TEST 123", rtty::DEFAULT_MARK_HZ, rtty::DEFAULT_SPACE_HZ);
        let mut pipe = DigitalPipeline::new(DigitalMode::Rtty, 48_000).unwrap();
        for chunk in audio.chunks(1024) {
            pipe.push_audio(chunk);
        }
        let decodes = pipe.drain_decodes();
        let text: String = decodes.iter().map(|d| d.text.as_str()).collect();
        assert!(text.contains("TEST 123"), "got: {text:?}");
    }

    #[test]
    fn pipeline_decodes_self_generated_psk31() {
        let audio = psk31::encode_text("hello world", DEFAULT_PSK_CENTER_HZ);
        let mut pipe = DigitalPipeline::new(DigitalMode::Psk31, 48_000).unwrap();
        // Feed in 1024-sample chunks to mimic the real DSP loop.
        for chunk in audio.chunks(1024) {
            pipe.push_audio(chunk);
        }
        let decodes = pipe.drain_decodes();
        let text: String = decodes.iter().map(|d| d.text.as_str()).collect();
        assert!(text.contains("hello world"), "got: {text:?}");
    }
}
