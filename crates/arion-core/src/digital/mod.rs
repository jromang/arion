//! Digital-mode pipeline.
//!
//! The DSP thread feeds post-AGC audio (48 kHz real) into a
//! `DigitalPipeline` per RX when the user selects a digital
//! decoder. A single [`ModeStage`] implementation owns the
//! mode-specific decoder, so adding a new mode is a matter of
//! shipping a new `impl ModeStage` — the pipeline itself, the
//! MVVM plumbing, and the UI don't change.

pub mod aprs;
pub mod ax25;
pub mod baudot;
pub mod psk31;
pub mod rtty;
pub mod varicode;
pub mod wspr;

use aprs::AprsDemod;
use liquid::{Complex32, MsResamp};
use psk31::Psk31Demod;
use rtty::RttyDemod;
use wspr::WsprDecoder;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum DigitalMode {
    Psk31,
    Psk63,
    Rtty,
    Aprs,
    Ft8,
    Wspr,
}

impl DigitalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Psk31 => "psk31",
            Self::Psk63 => "psk63",
            Self::Rtty => "rtty",
            Self::Aprs => "aprs",
            Self::Ft8 => "ft8",
            Self::Wspr => "wspr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "psk31" => Some(Self::Psk31),
            "psk63" => Some(Self::Psk63),
            "rtty" => Some(Self::Rtty),
            "aprs" => Some(Self::Aprs),
            "ft8" => Some(Self::Ft8),
            "wspr" => Some(Self::Wspr),
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

/// A mode-specific decoder plugged into a `DigitalPipeline`. One
/// instance per active RX. Implementations are private newtypes in
/// this module so the trait stays inside `arion-core`.
trait ModeStage: Send {
    /// Feed a block of post-AGC real audio (48 kHz). Any decoded
    /// messages are pushed into `out`.
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>);

    /// Snapshot the latest constellation points (I, Q). Default is
    /// empty; PSK-family decoders override this.
    fn constellation(&self) -> Vec<(f32, f32)> {
        Vec::new()
    }

    /// Retune the decoder to a new carrier offset in Hz (audio
    /// passband). Only meaningful for PSK-family modes; other modes
    /// use fixed tone pairs and ignore this.
    fn set_center_hz(&mut self, _hz: f32) {}
}

// --- Stage adapters around the per-mode demods -------------------------

struct Psk31Stage {
    demod: Psk31Demod,
    mode: DigitalMode, // Psk31 or Psk63
}

impl ModeStage for Psk31Stage {
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        self.demod.process_block(audio);
        let text = self.demod.drain_text();
        if !text.is_empty() {
            out.push(DigitalDecode {
                mode: self.mode,
                text,
                snr_db: 0.0,
                freq_hz: 0.0,
                time_offset_s: 0.0,
            });
        }
    }
    fn constellation(&self) -> Vec<(f32, f32)> {
        self.demod.constellation().to_vec()
    }
    fn set_center_hz(&mut self, hz: f32) {
        self.demod = Psk31Demod::new(hz);
    }
}

struct RttyStage {
    demod: RttyDemod,
}

impl ModeStage for RttyStage {
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        self.demod.process_block(audio);
        let text = self.demod.drain_text();
        if !text.is_empty() {
            out.push(DigitalDecode {
                mode: DigitalMode::Rtty,
                text,
                snr_db: 0.0,
                freq_hz: 0.0,
                time_offset_s: 0.0,
            });
        }
    }
}

struct WsprStage {
    decoder: WsprDecoder,
}

impl ModeStage for WsprStage {
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        for d in self.decoder.push_audio(audio) {
            out.push(wspr::to_digital_decode(&d));
        }
    }
}

struct AprsStage {
    demod: AprsDemod,
}

impl ModeStage for AprsStage {
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        self.demod.process_block(audio);
        for frame in self.demod.drain() {
            out.push(DigitalDecode {
                mode: DigitalMode::Aprs,
                text: format!("{}: {}", frame.header(), frame.info_str()),
                snr_db: 0.0,
                freq_hz: 0.0,
                time_offset_s: 0.0,
            });
        }
    }
}

/// Per-RX digital decoder pipeline.
pub struct DigitalPipeline {
    mode: DigitalMode,
    center_hz: f32,
    stage: Box<dyn ModeStage>,
    pending: Vec<DigitalDecode>,
}

/// FT8 decoder running inside the DigitalPipeline. Resamples 48 kHz
/// → 12 kHz, feeds a `ft8::Monitor` in 1920-sample (one-symbol)
/// blocks, and runs a decode every ~14 s of accumulated audio or at
/// each UTC 15-second slot boundary.
struct Ft8Stage {
    resampler: MsResamp,
    monitor: ft8::Monitor,
    scratch_in: Vec<Complex32>,
    scratch_out: Vec<Complex32>,
    pending_samples: Vec<f32>,
    samples_since_decode: usize,
    /// Last UTC slot index we triggered a decode for. FT8 slots are
    /// aligned to `unix_secs % 15 == 0`. When the slot index changes
    /// we flush and reset so decodes line up with real TX slots
    /// instead of an arbitrary rolling 14-second window.
    last_slot_idx: Option<u64>,
}

const FT8_DECODE_SAMPLES_12K: usize = 12_000 * 14; // fallback trigger if no wall-clock is available (tests).
const FT8_SLOT_SECS: u64 = 15;

fn current_ft8_slot_idx() -> Option<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() / FT8_SLOT_SECS)
}

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
            last_slot_idx: current_ft8_slot_idx(),
        })
    }

    fn drive(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
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
        // Prefer wall-clock slot alignment: when the UTC 15-second
        // slot index ticks over, flush and decode. Fall back to the
        // rolling 14-second counter if the clock is unavailable or
        // running faster than real time (tests).
        let slot_now = current_ft8_slot_idx();
        let slot_boundary = match (slot_now, self.last_slot_idx) {
            (Some(now), Some(last)) => now != last,
            _ => false,
        };
        let rolling_full = self.samples_since_decode >= FT8_DECODE_SAMPLES_12K;
        if slot_boundary || rolling_full {
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
            if let Some(idx) = slot_now {
                self.last_slot_idx = Some(idx);
            }
        }
    }
}

impl ModeStage for Ft8Stage {
    fn push_audio(&mut self, audio: &[f32], out: &mut Vec<DigitalDecode>) {
        self.drive(audio, out);
    }
}

const DEFAULT_PSK_CENTER_HZ: f32 = 1_500.0;

impl DigitalPipeline {
    pub fn new(mode: DigitalMode, _input_rate_hz: u32) -> Option<Self> {
        let stage: Box<dyn ModeStage> = match mode {
            DigitalMode::Psk31 | DigitalMode::Psk63 => Box::new(Psk31Stage {
                demod: Psk31Demod::new(DEFAULT_PSK_CENTER_HZ),
                mode,
            }),
            DigitalMode::Rtty => Box::new(RttyStage {
                demod: RttyDemod::new(rtty::DEFAULT_MARK_HZ, rtty::DEFAULT_SPACE_HZ),
            }),
            DigitalMode::Aprs => Box::new(AprsStage {
                demod: AprsDemod::new(),
            }),
            DigitalMode::Ft8 => Box::new(Ft8Stage::new()?),
            DigitalMode::Wspr => Box::new(WsprStage {
                decoder: WsprDecoder::new()?,
            }),
        };
        Some(Self {
            mode,
            center_hz: DEFAULT_PSK_CENTER_HZ,
            stage,
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
        self.stage.set_center_hz(hz);
    }

    /// Push a block of post-AGC real audio at 48 kHz. Decodes
    /// accumulate and drain via `drain_decodes`.
    pub fn push_audio(&mut self, audio: &[f32]) {
        self.stage.push_audio(audio, &mut self.pending);
    }

    pub fn drain_decodes(&mut self) -> Vec<DigitalDecode> {
        std::mem::take(&mut self.pending)
    }

    /// Snapshot the current constellation points (I, Q). Non-empty
    /// only for PSK-family decoders today.
    pub fn constellation(&self) -> Vec<(f32, f32)> {
        self.stage.constellation()
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
