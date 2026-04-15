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

pub mod aprs;
pub mod ax25;
pub mod baudot;
pub mod psk31;
pub mod rtty;
pub mod varicode;

use aprs::AprsDemod;
use liquid::{Complex32, MsResamp};
use psk31::Psk31Demod;
use rtty::RttyDemod;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DigitalMode {
    Psk31,
    Psk63,
    Rtty,
    Aprs,
    Ft8,
}

impl DigitalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Psk31 => "psk31",
            Self::Psk63 => "psk63",
            Self::Rtty => "rtty",
            Self::Aprs => "aprs",
            Self::Ft8 => "ft8",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "psk31" => Some(Self::Psk31),
            "psk63" => Some(Self::Psk63),
            "rtty" => Some(Self::Rtty),
            "aprs" => Some(Self::Aprs),
            "ft8" => Some(Self::Ft8),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DigitalDecode {
    pub mode: DigitalMode,
    pub text: String,
    /// Signal-quality indicator. For FT8 this is ft8_lib's sync score
    /// (not a true SNR but monotonic with it); for other modes it's 0
    /// until the demods learn to report one.
    pub snr_db: f32,
    /// Audio-passband frequency of the detected signal in Hz.
    /// 0.0 if the mode doesn't report per-decode frequencies.
    pub freq_hz: f32,
    /// Time offset within the decoded slot, in seconds.
    pub time_offset_s: f32,
}

/// Per-RX digital decoder pipeline.
pub struct DigitalPipeline {
    mode: DigitalMode,
    center_hz: f32,
    psk31: Option<Psk31Demod>,
    rtty: Option<RttyDemod>,
    aprs: Option<AprsDemod>,
    ft8: Option<Ft8Stage>,
    pending: Vec<DigitalDecode>,
}

/// FT8 decoder running inside the DigitalPipeline. Resamples 48 kHz
/// → 12 kHz, feeds a `ft8::Monitor` in 1920-sample (one-symbol)
/// blocks, and runs a decode every ~14 s of accumulated audio.
struct Ft8Stage {
    resampler: MsResamp,
    monitor: ft8::Monitor,
    scratch_in: Vec<Complex32>,
    scratch_out: Vec<Complex32>,
    pending_samples: Vec<f32>,
    samples_since_decode: usize,
}

const FT8_DECODE_SAMPLES_12K: usize = 12_000 * 14; // run a decode every ~14 s of accumulated 12 kHz audio.

impl Ft8Stage {
    fn new() -> Option<Self> {
        // 48 kHz real audio → 12 kHz complex for the ft8 monitor.
        let resampler = MsResamp::new(12_000.0 / 48_000.0, 60.0).ok()?;
        let monitor = ft8::Monitor::new().ok()?;
        Some(Self {
            resampler,
            monitor,
            scratch_in: Vec::with_capacity(2048),
            scratch_out: Vec::with_capacity(512),
            pending_samples: Vec::with_capacity(2048),
            samples_since_decode: 0,
        })
    }

    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        // Real-valued 48 k → complex; the monitor only uses the real
        // part of the mix-down internally, so feeding im=0 is fine.
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
        self.pending_samples
            .extend(self.scratch_out[..n].iter().map(|c| c.re));

        // Feed complete symbol-sized blocks to the monitor.
        let block = self.monitor.block_size();
        while self.pending_samples.len() >= block {
            let (head, _) = self.pending_samples.split_at(block);
            self.monitor.process(head);
            self.pending_samples.drain(..block);
        }

        self.samples_since_decode += n;
        if self.samples_since_decode >= FT8_DECODE_SAMPLES_12K {
            for d in self.monitor.decode(64, 10) {
                out.push(DigitalDecode {
                    mode: DigitalMode::Ft8,
                    text: d.text,
                    snr_db: d.score as f32,
                    freq_hz: d.freq_hz,
                    time_offset_s: d.time_offset_s,
                });
            }
            self.monitor.reset();
            self.samples_since_decode = 0;
        }
    }
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
        let aprs = match mode {
            DigitalMode::Aprs => Some(AprsDemod::new()),
            _ => None,
        };
        let ft8 = match mode {
            DigitalMode::Ft8 => Ft8Stage::new(),
            _ => None,
        };
        Some(Self {
            mode,
            center_hz: DEFAULT_PSK_CENTER_HZ,
            psk31,
            rtty,
            aprs,
            ft8,
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
        } else if let Some(demod) = self.aprs.as_mut() {
            demod.process_block(audio);
            demod
                .drain()
                .into_iter()
                .map(|f| format!("{}: {}\n", f.header(), f.info_str()))
                .collect::<String>()
        } else if let Some(stage) = self.ft8.as_mut() {
            stage.push_audio(audio, &mut self.pending);
            String::new()
        } else {
            String::new()
        };
        if !text.is_empty() {
            self.pending.push(DigitalDecode {
                mode: self.mode,
                text,
                snr_db: 0.0,
                freq_hz: self.center_hz,
                time_offset_s: 0.0,
            });
        }
    }

    pub fn drain_decodes(&mut self) -> Vec<DigitalDecode> {
        std::mem::take(&mut self.pending)
    }

    /// Snapshot the current constellation points (I, Q). Non-empty
    /// only for demods that expose them (PSK31 today).
    pub fn constellation(&self) -> Vec<(f32, f32)> {
        self.psk31
            .as_ref()
            .map(|d| d.constellation().to_vec())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_decodes_self_generated_ft8() {
        // Encode a 12 kHz FT8 signal, upsample to 48 kHz (the native
        // tap rate), then feed it to the DigitalPipeline — the same
        // path the DSP thread exercises.
        let audio_12k = ft8::encode_to_audio("CQ AA0AA FN42", 1_000.0).unwrap();
        // Crude 4× linear upsample just for the test — the pipeline's
        // own MsResamp takes us back to 12 kHz at its input. Real
        // off-air audio already lives at 48 kHz.
        let mut audio_48k = Vec::with_capacity(audio_12k.len() * 4);
        for s in &audio_12k {
            for _ in 0..4 {
                audio_48k.push(*s);
            }
        }
        let mut pipe = DigitalPipeline::new(DigitalMode::Ft8, 48_000).unwrap();
        for chunk in audio_48k.chunks(1024) {
            pipe.push_audio(chunk);
        }
        // Trigger a decode by flushing 14 s worth of silence (the
        // stage batches decodes every ~14 s of 12 kHz audio).
        let silence = vec![0.0_f32; 1024];
        for _ in 0..700 {
            pipe.push_audio(&silence);
        }
        let decodes = pipe.drain_decodes();
        let text: String = decodes.iter().map(|d| d.text.as_str()).collect();
        assert!(
            text.contains("AA0AA") || text.contains("FN42"),
            "got: {text:?}"
        );
    }

    #[test]
    fn pipeline_decodes_self_generated_aprs() {
        let frame = ax25::UiFrame {
            dest: ax25::Callsign::new("APZ000", 0),
            src: ax25::Callsign::new("F4XYZ", 0),
            info: b">hello aprs".to_vec(),
        };
        let audio = aprs::encode_frame(&frame);
        let mut pipe = DigitalPipeline::new(DigitalMode::Aprs, 48_000).unwrap();
        for chunk in audio.chunks(1024) {
            pipe.push_audio(chunk);
        }
        let decodes = pipe.drain_decodes();
        let text: String = decodes.iter().map(|d| d.text.as_str()).collect();
        assert!(text.contains("F4XYZ>APZ000"), "got: {text:?}");
        assert!(text.contains(">hello aprs"), "got: {text:?}");
    }

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
