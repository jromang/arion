//! Arion radio orchestration.
//!
//! Glues together:
//! - [`hpsdr_net::Session`]   — UDP transport to the HL2
//! - [`wdsp::Channel`]         — the DSP chain (demod, filter, AGC, …)
//! - [`arion_audio::AudioOutput`] — speakers
//!
//! Architecture:
//!
//! ```text
//!                network RX thread (hpsdr-net)
//!                        │  IqSample
//!                        ▼
//!              Consumer<IqSample> (rtrb)
//!                        │
//!  ┌─────────────────────┴──────────────────────────────┐
//!  │ arion-core DSP thread                             │
//!  │   ┌────────────┐      ┌──────────────┐             │
//!  │   │ IQ gather  │─────►│ wdsp::Channel│──► audio    │
//!  │   │ (1024 cplx)│      │   .process() │    .push()  │
//!  │   └────────────┘      └──────────────┘             │
//!  └────────────────────────┬───────────────────────────┘
//!                           ▼
//!                AudioSink (rtrb producer)
//!                           │  mono f32
//!                           ▼
//!               cpal output callback (arion-audio)
//!                           │
//!                           ▼
//!                       speakers
//! ```
//!
//! # Phase A scope
//!
//! - **One RX, no TX.**
//! - **48 kHz** pipeline end to end — HL2 IQ, WDSP dsp_rate, audio out.
//! - **Mono audio**: the DSP outputs complex pairs; we take the real part.
//! - **Parameter updates** (frequency, mode, gain) flow from the caller
//!   through an `mpsc::Sender` and are applied on the DSP thread between
//!   buffers, so the DSP loop never has to take a lock.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver as MpscRx, Sender as MpscTx};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use hpsdr_net::{Session, SessionConfig, SessionStatus};
use hpsdr_protocol::IqSample;
use rtrb::Consumer;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use arion_audio::{AudioConfig, AudioOutput, AudioSink};
use wdsp::{Channel as WdspChannel, Mode, RxConfig as WdspRxConfig};

// Re-exports so downstream crates only need to depend on `arion-core`.
pub use hpsdr_net::{discover, DiscoveryOptions, RadioInfo};
pub use hpsdr_net::session::MAX_SESSION_RX as MAX_RX;
pub use arion_audio::{list_output_devices, AudioStats, StereoFrame};
pub use wdsp::Mode as WdspMode;

pub mod digital;
pub use digital::{DigitalDecode, DigitalMode, DigitalPipeline};

/// Per-receiver configuration passed to [`Radio::start`].
#[derive(Debug, Clone, Copy)]
pub struct RxConfig {
    /// RX enabled on session startup. If `false`, the receiver's
    /// spectrum and audio contribution are suppressed but its DDC
    /// is still allocated on the radio — you can `set_rx_enabled`
    /// it back on at runtime without reconnecting.
    pub enabled: bool,
    /// Initial tuned frequency in Hz.
    pub frequency_hz: u32,
    /// Initial demodulation mode.
    pub mode: WdspMode,
    /// Linear gain applied after WDSP's panel gain, just before this
    /// RX's audio is mixed into the stereo bus.
    pub volume: f32,
}

impl Default for RxConfig {
    fn default() -> Self {
        RxConfig {
            enabled:      false,
            frequency_hz: 7_074_000,
            mode:         WdspMode::Usb,
            volume:       0.25,
        }
    }
}

/// Knobs for [`Radio::start`].
#[derive(Debug, Clone)]
pub struct RadioConfig {
    /// UDP endpoint of the HL2 (typically `<ip>:1024`).
    pub radio_addr: std::net::SocketAddr,
    /// Number of simultaneous receivers to start. Must be in
    /// `1..=MAX_RX`. HL2 only supports 2; larger radios go up to 7.
    pub num_rx: u8,
    /// Per-RX configuration. Only the first `num_rx` entries are
    /// consulted; the rest stay at [`RxConfig::default`].
    pub rx: [RxConfig; MAX_RX],
    /// Audio output device name. `None` uses the system default.
    pub audio_device: Option<String>,
    /// Whether to prime the FFTW wisdom cache before opening any
    /// WDSP channel. See the field doc on the phase-A `RadioConfig`
    /// for the test-vs-user trade-off.
    pub prime_wisdom: bool,
}

impl Default for RadioConfig {
    fn default() -> Self {
        let mut rx = [RxConfig::default(); MAX_RX];
        rx[0].enabled = true;
        RadioConfig {
            radio_addr:   "127.0.0.1:1024".parse().unwrap(),
            num_rx:       1,
            rx,
            audio_device: None,
            prime_wisdom: true,
        }
    }
}

/// Live counters exported from a running [`Radio`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RadioStatus {
    pub session:       SessionStatus,
    pub samples_dsp:   u64,
    pub samples_audio: u64,
    pub audio_underruns: u64,
}

/// Per-RX telemetry snapshot. One of these lives inside [`Telemetry`]
/// for every entry in the `rx` array.
#[derive(Debug, Clone)]
pub struct RxTelemetry {
    /// Whether this RX is currently contributing audio / spectrum. The
    /// UI should gray out the corresponding panel when this is false.
    pub enabled: bool,
    /// Log-magnitude FFT bins in dB, already `fftshift`ed so bin `0`
    /// is the lowest frequency and bin `N-1` the highest. Length is
    /// [`SPECTRUM_BINS`].
    pub spectrum_bins_db: Vec<f32>,
    /// Post-DSP audio RMS, in dB relative to full scale (0 dBFS = sine
    /// at ±1.0). Exponentially smoothed for readable display.
    pub s_meter_db: f32,
    /// Center frequency in Hz at the time this snapshot was taken.
    pub center_freq_hz: u32,
    /// Full spectral span covered by the FFT (= input sample rate).
    pub span_hz: u32,
    /// Current demodulation mode.
    pub mode: WdspMode,
    /// Active digital decoder mode, if any (PSK31/RTTY/APRS).
    pub digital_mode: Option<DigitalMode>,
    /// Digital decodes emitted since the last snapshot. Empty if no
    /// digital decoder is active. Small, bounded ring.
    pub digital_decodes: Vec<DigitalDecode>,
    /// Latest constellation points (I, Q) captured by a PSK-family
    /// demod, in capture order. Empty for non-constellation modes.
    pub constellation: Vec<(f32, f32)>,
}

impl Default for RxTelemetry {
    fn default() -> Self {
        RxTelemetry {
            enabled:          false,
            spectrum_bins_db: vec![-140.0; SPECTRUM_BINS],
            s_meter_db:       -140.0,
            center_freq_hz:   0,
            span_hz:          48_000,
            mode:             WdspMode::Usb,
            digital_mode:     None,
            digital_decodes:  Vec::new(),
            constellation:    Vec::new(),
        }
    }
}

/// A frozen snapshot of everything the UI wants to render in one frame.
///
/// Published atomically from the DSP thread via [`arc_swap::ArcSwap`];
/// the UI side loads the latest `Arc<Telemetry>` with a single
/// lock-free read and can hold on to it for the rest of the frame
/// without blocking the producer.
#[derive(Debug, Clone)]
pub struct Telemetry {
    /// Per-receiver telemetry. Only `rx[0..num_rx as usize]` holds
    /// live data; the rest is the default sentinel.
    pub rx: [RxTelemetry; MAX_RX],
    /// Number of configured receivers.
    pub num_rx: u8,
    /// Monotonic timestamp of the last DSP frame. Lets the UI detect
    /// a stalled pipeline.
    pub last_update: Instant,
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry {
            rx:          std::array::from_fn(|_| RxTelemetry::default()),
            num_rx:      1,
            last_update: Instant::now(),
        }
    }
}

/// Fixed FFT size for the spectrum display. Matches the WDSP RX
/// `in_size` so we can reuse the DSP thread's input buffer directly.
pub const SPECTRUM_BINS: usize = 1024;

/// How often we republish a spectrum snapshot. The DSP thread runs at
/// ~47 buffers/sec (48 kHz / 1024 samples); refreshing the UI every
/// other buffer ≈ 23 Hz is visually smooth without burning GPU.
const SPECTRUM_UPDATE_INTERVAL: Duration = Duration::from_millis(40);

/// Commands applied to a live radio from outside the DSP thread.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum DspCommand {
    SetRxMode { rx: u8, mode: Mode },
    SetRxVolume { rx: u8, volume: f32 },
    SetRxEnabled { rx: u8, enabled: bool },
    SetRxNr3 { rx: u8, enabled: bool },
    SetRxNr4 { rx: u8, enabled: bool },
    SetRxPassband { rx: u8, lo: f64, hi: f64 },
    SetRxEqRun { rx: u8, enabled: bool },
    SetRxEqBands { rx: u8, gains: [i32; 11] },
    SetRxAnf { rx: u8, enabled: bool },
    SetRxSnba { rx: u8, enabled: bool },
    SetRxEmnr { rx: u8, enabled: bool },
    SetRxAnr  { rx: u8, enabled: bool },
    SetRxBinaural { rx: u8, enabled: bool },
    // --- Phase E.10–E.13 ---
    SetRxSquelchRun       { rx: u8, enabled: bool },
    SetRxSquelchThreshold { rx: u8, threshold: f64 },
    SetRxApfRun           { rx: u8, enabled: bool },
    SetRxApfFreq          { rx: u8, freq_hz: f64 },
    SetRxApfBandwidth     { rx: u8, bw_hz:   f64 },
    SetRxApfGain          { rx: u8, gain_db: f64 },
    SetRxAgcTop           { rx: u8, dbm:     f64 },
    SetRxAgcHangLevel     { rx: u8, level:   f64 },
    SetRxAgcDecay         { rx: u8, decay_ms: i32 },
    SetRxAgcFixedGain     { rx: u8, gain_db: f64 },
    SetRxFmDeviation      { rx: u8, hz:      f64 },
    SetRxCtcssRun         { rx: u8, enabled: bool },
    SetRxCtcssFreq        { rx: u8, hz:      f64 },
    SetRxNb               { rx: u8, enabled: bool },
    SetRxNb2              { rx: u8, enabled: bool },
    SetRxNbThreshold      { rx: u8, threshold: f64 },
    SetRxNb2Threshold     { rx: u8, threshold: f64 },
    // --- TNF + SAM ---
    SetRxTnfEnabled       { rx: u8, enabled: bool },
    AddRxTnfNotch         { rx: u8, idx: u32, freq_hz: f64, width_hz: f64, active: bool },
    EditRxTnfNotch        { rx: u8, idx: u32, freq_hz: f64, width_hz: f64, active: bool },
    DeleteRxTnfNotch      { rx: u8, idx: u32 },
    SetRxSamSubmode       { rx: u8, submode: u8 },
    SetRxBpsnbaNc         { rx: u8, nc: u32 },
    SetRxBpsnbaMp         { rx: u8, mp: bool },
    SetRxDigitalMode      { rx: u8, mode: Option<DigitalMode> },
}

/// A running end-to-end receive session.
///
/// Dropping the handle stops the DSP thread, the network session, and
/// the audio stream cleanly.
pub struct Radio {
    session:     Option<Session>,
    audio:       Option<AudioOutput>,
    dsp_thread:  Option<JoinHandle<()>>,
    shutdown:    Arc<AtomicBool>,
    commands:    MpscTx<DspCommand>,
    /// Side-channel for the secondary audio tap producer. Kept
    /// separate from [`DspCommand`] because `rtrb::Producer` is
    /// `!Sync` and can't ride through the main command channel
    /// without breaking other `anyhow` callsites.
    audio_tap_tx: MpscTx<Option<rtrb::Producer<StereoFrame>>>,
    samples_dsp: Arc<std::sync::atomic::AtomicU64>,
    telemetry:   Arc<ArcSwap<Telemetry>>,
    /// One atomic u32 per RX so the UI can read the live tuned
    /// frequency without locking. Written by `Radio::set_rx_frequency`
    /// and also refreshed at every telemetry snapshot.
    center_freqs: [Arc<std::sync::atomic::AtomicU32>; MAX_RX],
    num_rx:      u8,
}

impl Radio {
    /// Start a receiver against the given radio.
    ///
    /// Sequence:
    /// 1. Spin up the `hpsdr-net` session (includes the Start handshake).
    /// 2. Open a WDSP channel tuned to the requested mode.
    /// 3. Open the audio output.
    /// 4. Start the DSP thread that reads IQ → `fexchange0` → audio.
    /// 5. Set the initial frequency via [`Session::set_rx_frequency`].
    pub fn start(config: RadioConfig) -> anyhow::Result<Self> {
        tracing::info!(?config, "starting Radio");
        let num_rx = config.num_rx as usize;
        anyhow::ensure!(
            (1..=MAX_RX).contains(&num_rx),
            "num_rx must be in 1..={MAX_RX}, got {num_rx}"
        );

        // --- Prime FFTW wisdom before opening any WDSP channel ------
        //
        // Uses the embedded-blob path so a fresh install (no
        // user-local `wdspWisdom00`) gets seeded with the pre-built
        // wisdom file at compile time. Without it, FFTW would spend
        // 1–10 minutes building plans on the very first launch.
        if config.prime_wisdom {
            match wdsp::prime_wisdom_with_embedded_default() {
                Ok(Some(status)) => tracing::info!(?status, "FFTW wisdom primed"),
                Ok(None) => tracing::debug!("no wisdom cache dir, skipping"),
                Err(e) => tracing::warn!(error = %e, "wisdom prime failed, continuing"),
            }
        }

        // --- Network session ----------------------------------------
        let mut session_config = SessionConfig {
            radio_addr:        config.radio_addr,
            num_rx:            config.num_rx,
            sample_rate_index: 0, // 48 kHz
            ring_capacity:     32_768,
            start_timeout:     Duration::from_secs(2),
            ..SessionConfig::default()
        };
        for r in 0..num_rx {
            session_config.rx_frequencies[r] = config.rx[r].frequency_hz;
        }
        let (session, consumers) = Session::start(session_config)?;
        anyhow::ensure!(
            consumers.len() == num_rx,
            "session returned {} consumers, expected {}",
            consumers.len(), num_rx
        );

        // --- WDSP channels ------------------------------------------
        //
        // One WDSP channel per RX, indexed 0..num_rx. FFTW plan reuse
        // across opens is handled by the wisdom cache (B.1.1) so
        // opening N channels sequentially is only slow on the very
        // first uncached run.
        let mut channels: Vec<WdspChannel> = Vec::with_capacity(num_rx);
        for r in 0..num_rx {
            let rx_cfg = WdspRxConfig {
                id:   r as i32,
                mode: config.rx[r].mode,
                ..WdspRxConfig::default()
            };
            channels.push(WdspChannel::open_rx(rx_cfg)?);
        }
        let in_size = channels[0].in_size();

        // --- Audio output -------------------------------------------
        let (audio_out, audio_sink) = AudioOutput::start(AudioConfig {
            device_name:   config.audio_device.clone(),
            sample_rate:   48_000,
            ring_capacity: 16_384,
        })?;

        // --- DSP thread ---------------------------------------------
        let shutdown = Arc::new(AtomicBool::new(false));
        let samples_dsp = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (command_tx, command_rx) = mpsc::channel::<DspCommand>();
        let (audio_tap_tx, audio_tap_rx) =
            mpsc::channel::<Option<rtrb::Producer<StereoFrame>>>();

        let center_freqs: [Arc<std::sync::atomic::AtomicU32>; MAX_RX] =
            std::array::from_fn(|r| {
                let hz = if r < num_rx { config.rx[r].frequency_hz } else { 0 };
                Arc::new(std::sync::atomic::AtomicU32::new(hz))
            });

        let mut initial_telemetry = Telemetry {
            num_rx: config.num_rx,
            ..Telemetry::default()
        };
        for r in 0..num_rx {
            initial_telemetry.rx[r].enabled        = config.rx[r].enabled;
            initial_telemetry.rx[r].center_freq_hz = config.rx[r].frequency_hz;
            initial_telemetry.rx[r].mode           = config.rx[r].mode;
        }
        let telemetry = Arc::new(ArcSwap::from_pointee(initial_telemetry));

        // Snapshot per-RX initial state for the DSP thread.
        let initial_rx: [RxRuntime; MAX_RX] =
            std::array::from_fn(|r| RxRuntime {
                enabled:      r < num_rx && config.rx[r].enabled,
                volume:       config.rx[r].volume,
                mode:         config.rx[r].mode,
                digital_mode: None,
            });

        let dsp_thread = {
            let shutdown     = Arc::clone(&shutdown);
            let samples_dsp  = Arc::clone(&samples_dsp);
            let telemetry    = Arc::clone(&telemetry);
            let center_freqs = center_freqs.clone();
            thread::Builder::new()
                .name("arion-dsp".into())
                .spawn(move || {
                    dsp_loop(
                        channels,
                        consumers,
                        num_rx,
                        initial_rx,
                        audio_sink,
                        command_rx,
                        audio_tap_rx,
                        shutdown,
                        samples_dsp,
                        telemetry,
                        center_freqs,
                        in_size,
                    )
                })?
        };

        Ok(Radio {
            session:     Some(session),
            audio:       Some(audio_out),
            dsp_thread:  Some(dsp_thread),
            shutdown,
            commands:    command_tx,
            audio_tap_tx,
            samples_dsp,
            telemetry,
            center_freqs,
            num_rx:      config.num_rx,
        })
    }

    pub fn num_rx(&self) -> u8 { self.num_rx }

    /// Shared handle to the latest telemetry snapshot. The UI calls
    /// `radio.telemetry().load()` once per frame; the DSP thread
    /// publishes new snapshots via
    /// [`arc_swap::ArcSwap::store`].
    pub fn telemetry(&self) -> Arc<ArcSwap<Telemetry>> {
        Arc::clone(&self.telemetry)
    }

    /// Ask the radio to retune the given RX. Propagated to the radio
    /// on the next TX keep-alive tick. Also bumps the atomic the DSP
    /// thread reads when stamping spectrum snapshots so the UI
    /// frequency label updates immediately.
    pub fn set_rx_frequency(&self, rx: u8, hz: u32) -> anyhow::Result<()> {
        let r = rx as usize;
        anyhow::ensure!(r < self.num_rx as usize, "rx {rx} out of range");
        if let Some(s) = &self.session {
            s.set_rx_frequency(rx, hz)?;
        }
        self.center_freqs[r].store(hz, Ordering::Release);
        Ok(())
    }

    /// Back-compat convenience: retune RX0.
    pub fn set_frequency(&self, hz: u32) -> anyhow::Result<()> {
        self.set_rx_frequency(0, hz)
    }

    /// Change demodulation mode for a given RX.
    pub fn set_rx_mode(&self, rx: u8, mode: Mode) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxMode { rx, mode })?;
        Ok(())
    }

    /// Back-compat: set RX0 mode.
    pub fn set_mode(&self, mode: Mode) -> anyhow::Result<()> {
        self.set_rx_mode(0, mode)
    }

    /// Enable or disable a digital decoder on top of the analog DSP
    /// pipeline for a given RX. `None` disables any active decoder.
    pub fn set_rx_digital_mode(
        &self,
        rx: u8,
        mode: Option<DigitalMode>,
    ) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxDigitalMode { rx, mode })?;
        Ok(())
    }

    /// Set the post-DSP linear audio gain for a given RX.
    pub fn set_rx_volume(&self, rx: u8, volume: f32) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxVolume { rx, volume })?;
        Ok(())
    }

    /// Back-compat: set RX0 volume.
    pub fn set_volume(&self, linear: f32) -> anyhow::Result<()> {
        self.set_rx_volume(0, linear)
    }

    /// Enable / disable a receiver's contribution to the audio mix and
    /// spectrum. The WDSP channel keeps processing in either case.
    pub fn set_rx_enabled(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxEnabled { rx, enabled })?;
        Ok(())
    }

    /// Enable / disable the RNNoise (NR3) denoiser on a receiver. No-op
    /// if `wdsp-sys` was built without `rnnoise` pkg-config detection
    /// (the call is forwarded to an empty stub inside WDSP).
    pub fn set_rx_nr3(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNr3 { rx, enabled })?;
        Ok(())
    }

    /// Enable / disable the libspecbleach (NR4) adaptive denoiser on a
    /// receiver. Same build-time best-effort semantics as
    /// [`Self::set_rx_nr3`].
    pub fn set_rx_nr4(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNr4 { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_passband(&self, rx: u8, lo: f64, hi: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxPassband { rx, lo, hi })?;
        Ok(())
    }

    pub fn set_rx_eq_run(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxEqRun { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_eq_bands(&self, rx: u8, gains: [i32; 11]) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxEqBands { rx, gains })?;
        Ok(())
    }

    pub fn set_rx_anf(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAnf { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_snba(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxSnba { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_emnr(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxEmnr { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_anr(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAnr { rx, enabled })?;
        Ok(())
    }

    pub fn set_rx_squelch_run(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxSquelchRun { rx, enabled })?;
        Ok(())
    }
    pub fn set_rx_squelch_threshold(&self, rx: u8, threshold: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxSquelchThreshold { rx, threshold })?;
        Ok(())
    }
    pub fn set_rx_apf_run(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxApfRun { rx, enabled })?;
        Ok(())
    }
    pub fn set_rx_apf_freq(&self, rx: u8, hz: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxApfFreq { rx, freq_hz: hz })?;
        Ok(())
    }
    pub fn set_rx_apf_bandwidth(&self, rx: u8, hz: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxApfBandwidth { rx, bw_hz: hz })?;
        Ok(())
    }
    pub fn set_rx_apf_gain(&self, rx: u8, gain_db: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxApfGain { rx, gain_db })?;
        Ok(())
    }
    pub fn set_rx_agc_top(&self, rx: u8, dbm: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAgcTop { rx, dbm })?;
        Ok(())
    }
    pub fn set_rx_agc_hang_level(&self, rx: u8, level: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAgcHangLevel { rx, level })?;
        Ok(())
    }
    pub fn set_rx_agc_decay(&self, rx: u8, decay_ms: i32) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAgcDecay { rx, decay_ms })?;
        Ok(())
    }
    pub fn set_rx_agc_fixed_gain(&self, rx: u8, gain_db: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxAgcFixedGain { rx, gain_db })?;
        Ok(())
    }
    pub fn set_rx_fm_deviation(&self, rx: u8, hz: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxFmDeviation { rx, hz })?;
        Ok(())
    }
    pub fn set_rx_ctcss_run(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxCtcssRun { rx, enabled })?;
        Ok(())
    }
    pub fn set_rx_ctcss_freq(&self, rx: u8, hz: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxCtcssFreq { rx, hz })?;
        Ok(())
    }

    pub fn set_rx_nb(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNb { rx, enabled })?;
        Ok(())
    }
    pub fn set_rx_nb2(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNb2 { rx, enabled })?;
        Ok(())
    }
    pub fn set_rx_nb_threshold(&self, rx: u8, threshold: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNbThreshold { rx, threshold })?;
        Ok(())
    }
    pub fn set_rx_nb2_threshold(&self, rx: u8, threshold: f64) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxNb2Threshold { rx, threshold })?;
        Ok(())
    }

    pub fn set_rx_tnf_enabled(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxTnfEnabled { rx, enabled })?;
        Ok(())
    }
    pub fn add_rx_tnf_notch(&self, rx: u8, idx: u32, freq_hz: f64, width_hz: f64, active: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::AddRxTnfNotch { rx, idx, freq_hz, width_hz, active })?;
        Ok(())
    }
    pub fn edit_rx_tnf_notch(&self, rx: u8, idx: u32, freq_hz: f64, width_hz: f64, active: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::EditRxTnfNotch { rx, idx, freq_hz, width_hz, active })?;
        Ok(())
    }
    pub fn delete_rx_tnf_notch(&self, rx: u8, idx: u32) -> anyhow::Result<()> {
        self.commands.send(DspCommand::DeleteRxTnfNotch { rx, idx })?;
        Ok(())
    }
    pub fn set_rx_sam_submode(&self, rx: u8, submode: u8) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxSamSubmode { rx, submode })?;
        Ok(())
    }
    pub fn set_rx_bpsnba_nc(&self, rx: u8, nc: u32) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxBpsnbaNc { rx, nc })?;
        Ok(())
    }
    pub fn set_rx_bpsnba_mp(&self, rx: u8, mp: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxBpsnbaMp { rx, mp })?;
        Ok(())
    }

    pub fn set_rx_binaural(&self, rx: u8, enabled: bool) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetRxBinaural { rx, enabled })?;
        Ok(())
    }

    /// Attach or detach a secondary stereo audio tap. The DSP thread
    /// mirrors every frame it sends to the cpal output into `producer`
    /// using a non-blocking push — drops samples silently if the ring
    /// fills up. Pass `None` to detach the current tap.
    ///
    /// Intended for out-of-band consumers like `arion-web` (WebRTC
    /// Opus encoder); does not affect the primary audio path.
    pub fn set_audio_tap(
        &self,
        producer: Option<rtrb::Producer<StereoFrame>>,
    ) -> anyhow::Result<()> {
        self.audio_tap_tx
            .send(producer)
            .map_err(|_| anyhow::anyhow!("DSP thread is gone; cannot attach audio tap"))?;
        Ok(())
    }

    pub fn status(&self) -> RadioStatus {
        let session = self
            .session
            .as_ref()
            .map(|s| s.status())
            .unwrap_or_default();
        let (samples_audio, audio_underruns) = self
            .audio
            .as_ref()
            .map(|a| {
                let s = a.stats();
                (
                    s.samples_played.load(Ordering::Relaxed),
                    s.underruns.load(Ordering::Relaxed),
                )
            })
            .unwrap_or((0, 0));
        RadioStatus {
            session,
            samples_dsp: self.samples_dsp.load(Ordering::Relaxed),
            samples_audio,
            audio_underruns,
        }
    }

    /// Stop the pipeline in order: DSP thread, network session, audio.
    pub fn stop(mut self) -> anyhow::Result<()> {
        self.do_stop()
    }

    fn do_stop(&mut self) -> anyhow::Result<()> {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return Ok(()); // already stopped
        }
        if let Some(t) = self.dsp_thread.take() {
            let _ = t.join();
        }
        if let Some(s) = self.session.take() {
            let _ = s.stop();
        }
        // AudioOutput is just dropped — its cpal::Stream does the right
        // thing on drop.
        drop(self.audio.take());
        tracing::info!("Radio stopped");
        Ok(())
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        let _ = self.do_stop();
    }
}

/// Snapshot of per-RX runtime state used by the DSP loop. Kept simple
/// (Copy) so the main loop can mutate entries in place without having
/// to worry about borrows.
#[derive(Debug, Clone, Copy)]
struct RxRuntime {
    enabled:      bool,
    volume:       f32,
    mode:         Mode,
    digital_mode: Option<DigitalMode>,
}

/// Raise the DSP thread to a real-time scheduling class so audio
/// processing isn't preempted by the desktop compositor, browser
/// tabs, or any other `SCHED_OTHER` load.
///
/// We bypass `thread-priority`'s default behaviour because it has
/// a subtle footgun on Linux: `set_current_thread_priority` first
/// calls `thread_schedule_policy()` which returns the thread's
/// *current* policy (`SCHED_OTHER` by default), then asks the OS
/// to change the priority under that policy — and `SCHED_OTHER`'s
/// only valid static priority is zero. Result: `EPERM` / `EINVAL`
/// for every non-trivial priority request, even when the user has
/// `RLIMIT_RTPRIO` 99 properly configured via `@audio` group +
/// `/etc/security/limits.d/`.
///
/// Instead, we explicitly request `SCHED_FIFO` with priority 80.
/// Why 80: the Linux convention is to leave 90–99 to kernel RT
/// threads (softirqs, irqbalance) and to audio-server critical
/// paths like PipeWire's pulse bridge, so 80 is high enough to
/// preempt the desktop but polite to the OS.
///
/// Fallbacks:
///   1. `SCHED_FIFO` prio 80 — the real fix.
///   2. `SCHED_RR` prio 80 — same effective priority, used if
///      FIFO is unavailable (mostly a Windows thing).
///   3. Log at `info!` level and keep running. The wide rtrb
///      rings cover for most scheduler jitter on an idle machine.
#[cfg(windows)]
fn raise_dsp_thread_priority() {
    // Windows doesn't expose SCHED_FIFO — the closest equivalent is
    // THREAD_PRIORITY_TIME_CRITICAL, which `thread_priority::Max`
    // maps onto. That's more than enough to preempt desktop apps
    // under normal load and keeps us off the MMCSS "Pro Audio" tier
    // (reserved for WASAPI exclusive-mode audio).
    use thread_priority::{set_current_thread_priority, ThreadPriority};

    match set_current_thread_priority(ThreadPriority::Max) {
        Ok(()) => tracing::info!("DSP thread running at THREAD_PRIORITY_TIME_CRITICAL"),
        Err(e) => tracing::warn!("could not raise DSP thread priority on Windows: {e:?}"),
    }
}

#[cfg(unix)]
fn raise_dsp_thread_priority() {
    use thread_priority::{
        set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy,
        ThreadPriority, ThreadPriorityValue, ThreadSchedulePolicy,
    };

    // Linux SCHED_FIFO accepts priorities 1..=99. 80 is a common
    // upper-middle value: high enough to preempt SCHED_OTHER, low
    // enough to leave headroom for kernel RT threads at 90+.
    const RT_PRIORITY: u8 = 80;

    let native = thread_native_id();
    let prio_value = ThreadPriorityValue::try_from(RT_PRIORITY)
        .expect("80 fits in the valid ThreadPriorityValue range");

    let fifo = ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo);
    if set_thread_priority_and_policy(
        native,
        ThreadPriority::Crossplatform(prio_value),
        fifo,
    )
    .is_ok()
    {
        tracing::info!(
            priority = RT_PRIORITY,
            "DSP thread running at SCHED_FIFO/{}",
            RT_PRIORITY
        );
        return;
    }

    let rr = ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::RoundRobin);
    if set_thread_priority_and_policy(
        native,
        ThreadPriority::Crossplatform(prio_value),
        rr,
    )
    .is_ok()
    {
        tracing::info!(
            priority = RT_PRIORITY,
            "DSP thread running at SCHED_RR/{}",
            RT_PRIORITY
        );
        return;
    }

    tracing::warn!(
        "could not raise DSP thread priority: neither SCHED_FIFO nor \
         SCHED_RR accepted. On Linux, join the `audio` group and ensure \
         `/etc/security/limits.d/*-audio.conf` contains `@audio - rtprio 99`, \
         then log out and back in."
    );
}

/// Main DSP loop. Runs one thread for every WDSP channel we opened.
///
/// Per buffer:
/// 1. Drain pending [`DspCommand`]s.
/// 2. For each RX, gather `in_size` IQ samples from its consumer.
/// 3. For each RX, run WDSP `fexchange0` to produce demodulated output.
/// 4. Mix into a stereo bus: RX0 → L, RX1 → R, others → mixed into L.
/// 5. Every [`SPECTRUM_UPDATE_INTERVAL`], compute a per-RX FFT and
///    publish a new [`Telemetry`] snapshot.
#[allow(clippy::too_many_arguments)]
fn dsp_loop(
    mut channels: Vec<WdspChannel>,
    mut consumers: Vec<Consumer<IqSample>>,
    num_rx: usize,
    initial_rx: [RxRuntime; MAX_RX],
    mut audio: AudioSink,
    commands:  MpscRx<DspCommand>,
    audio_tap_rx: MpscRx<Option<rtrb::Producer<StereoFrame>>>,
    shutdown:  Arc<AtomicBool>,
    samples_dsp: Arc<std::sync::atomic::AtomicU64>,
    telemetry:   Arc<ArcSwap<Telemetry>>,
    center_freqs: [Arc<std::sync::atomic::AtomicU32>; MAX_RX],
    in_size:     usize,
) {
    raise_dsp_thread_priority();

    // Per-RX exchange buffers.
    let mut wdsp_in:  Vec<Vec<f64>> = (0..num_rx).map(|_| vec![0.0; 2 * in_size]).collect();
    let mut wdsp_out: Vec<Vec<f64>> = (0..num_rx).map(|_| vec![0.0; 2 * in_size]).collect();
    let mut gathered:  Vec<usize>   = vec![0usize; num_rx];

    // Stereo audio burst scratch — one entry per input sample.
    let mut audio_burst: Vec<StereoFrame> = vec![[0.0; 2]; in_size];

    // Per-RX runtime (volume, enabled, mode).
    let mut rx_state: [RxRuntime; MAX_RX] = initial_rx;

    // --- Spectrum state (shared across RXs since all use the same FFT size) ---
    let mut planner = FftPlanner::<f32>::new();
    let fft: Arc<dyn Fft<f32>> = planner.plan_fft_forward(in_size);
    let hann: Vec<f32> = (0..in_size)
        .map(|n| {
            let x = std::f32::consts::PI * (n as f32) / ((in_size - 1) as f32);
            x.sin().powi(2)
        })
        .collect();
    let fft_norm = 1.0_f32 / (in_size as f32).sqrt();
    let mut fft_buf: Vec<Complex32> = vec![Complex32::new(0.0, 0.0); in_size];
    let mut fft_scratch: Vec<Complex32> =
        vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];

    let mut smoothed_s_meter_db: [f32; MAX_RX] = [-140.0; MAX_RX];
    let mut last_spectrum_push = Instant::now() - SPECTRUM_UPDATE_INTERVAL;

    // Secondary stereo tap (e.g. arion-web → WebRTC). None by default;
    // swapped in/out via `DspCommand::SetAudioTap`. Non-blocking push,
    // drops samples if the consumer side falls behind rather than
    // stalling DSP.
    let mut audio_tap: Option<rtrb::Producer<StereoFrame>> = None;

    // Per-RX digital decoder pipelines. `None` = no digital mode
    // active on that RX. Created/destroyed by SetRxDigitalMode.
    let mut digital: [Option<DigitalPipeline>; MAX_RX] = std::array::from_fn(|_| None);
    let mut digital_audio_buf: Vec<f32> = Vec::with_capacity(4096);
    let mut pending_decodes: [Vec<DigitalDecode>; MAX_RX] = std::array::from_fn(|_| Vec::new());

    while !shutdown.load(Ordering::Acquire) {
        // 1. Apply any pending commands before starting a new buffer.
        while let Ok(cmd) = commands.try_recv() {
            match cmd {
                DspCommand::SetRxMode { rx, mode } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, new_mode = ?mode, "DSP: mode change");
                        channels[r].set_mode(mode);
                        rx_state[r].mode = mode;
                    }
                }
                DspCommand::SetRxVolume { rx, volume } => {
                    let r = rx as usize;
                    if r < num_rx {
                        rx_state[r].volume = volume;
                    }
                }
                DspCommand::SetRxEnabled { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        rx_state[r].enabled = enabled;
                    }
                }
                DspCommand::SetRxNr3 { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: NR3 toggle");
                        channels[r].set_nr3_enabled(enabled);
                    }
                }
                DspCommand::SetRxNr4 { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: NR4 toggle");
                        channels[r].set_nr4_enabled(enabled);
                    }
                }
                DspCommand::SetRxPassband { rx, lo, hi } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, lo, hi, "DSP: passband change");
                        channels[r].set_passband_hz(lo, hi);
                    }
                }
                DspCommand::SetRxEqRun { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: EQ run");
                        channels[r].set_eq_enabled(enabled);
                    }
                }
                DspCommand::SetRxEqBands { rx, gains } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, ?gains, "DSP: EQ bands");
                        channels[r].set_eq_bands(&gains);
                    }
                }
                DspCommand::SetRxAnf { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: ANF toggle");
                        channels[r].set_anf_enabled(enabled);
                    }
                }
                DspCommand::SetRxSnba { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: SNBA toggle");
                        channels[r].set_snba_enabled(enabled);
                    }
                }
                DspCommand::SetRxEmnr { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: EMNR toggle");
                        channels[r].set_emnr_enabled(enabled);
                    }
                }
                DspCommand::SetRxAnr { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: ANR toggle");
                        channels[r].set_anr_enabled(enabled);
                    }
                }
                DspCommand::SetRxBinaural { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: binaural toggle");
                        channels[r].set_binaural(enabled);
                    }
                }
                DspCommand::SetRxSquelchRun { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        let m = channels[r].mode();
                        channels[r].set_squelch_enabled(m, enabled);
                    }
                }
                DspCommand::SetRxSquelchThreshold { rx, threshold } => {
                    let r = rx as usize;
                    if r < num_rx {
                        let m = channels[r].mode();
                        channels[r].set_squelch_threshold(m, threshold);
                    }
                }
                DspCommand::SetRxApfRun { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_apf_enabled(enabled); }
                }
                DspCommand::SetRxApfFreq { rx, freq_hz } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_apf_freq(freq_hz); }
                }
                DspCommand::SetRxApfBandwidth { rx, bw_hz } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_apf_bandwidth(bw_hz); }
                }
                DspCommand::SetRxApfGain { rx, gain_db } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_apf_gain(gain_db); }
                }
                DspCommand::SetRxAgcTop { rx, dbm } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_agc_top(dbm); }
                }
                DspCommand::SetRxAgcHangLevel { rx, level } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_agc_hang_level(level); }
                }
                DspCommand::SetRxAgcDecay { rx, decay_ms } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_agc_decay(decay_ms); }
                }
                DspCommand::SetRxAgcFixedGain { rx, gain_db } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_agc_fixed_gain(gain_db); }
                }
                DspCommand::SetRxFmDeviation { rx, hz } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_fm_deviation(hz); }
                }
                DspCommand::SetRxCtcssRun { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_ctcss_enabled(enabled); }
                }
                DspCommand::SetRxCtcssFreq { rx, hz } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_ctcss_freq(hz); }
                }
                DspCommand::SetRxNb { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: NB toggle");
                        channels[r].set_nb_enabled(enabled);
                    }
                }
                DspCommand::SetRxNb2 { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, enabled, "DSP: NB2 toggle");
                        channels[r].set_nb2_enabled(enabled);
                    }
                }
                DspCommand::SetRxNbThreshold { rx, threshold } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_nb_threshold(threshold); }
                }
                DspCommand::SetRxNb2Threshold { rx, threshold } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_nb2_threshold(threshold); }
                }
                DspCommand::SetRxTnfEnabled { rx, enabled } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_tnf_enabled(enabled); }
                }
                DspCommand::AddRxTnfNotch { rx, idx, freq_hz, width_hz, active } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].add_tnf_notch(idx, freq_hz, width_hz, active); }
                }
                DspCommand::EditRxTnfNotch { rx, idx, freq_hz, width_hz, active } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].edit_tnf_notch(idx, freq_hz, width_hz, active); }
                }
                DspCommand::DeleteRxTnfNotch { rx, idx } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].delete_tnf_notch(idx); }
                }
                DspCommand::SetRxSamSubmode { rx, submode } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_sam_submode(submode); }
                }
                DspCommand::SetRxBpsnbaNc { rx, nc } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_bpsnba_nc(nc); }
                }
                DspCommand::SetRxBpsnbaMp { rx, mp } => {
                    let r = rx as usize;
                    if r < num_rx { channels[r].set_bpsnba_mp(mp); }
                }
                DspCommand::SetRxDigitalMode { rx, mode } => {
                    let r = rx as usize;
                    if r < num_rx {
                        tracing::info!(rx, ?mode, "DSP: digital mode change");
                        rx_state[r].digital_mode = mode;
                        digital[r] = mode
                            .and_then(|m| DigitalPipeline::new(m, 48_000));
                        pending_decodes[r].clear();
                    }
                }
            }
        }

        // Drain audio-tap updates (separate channel because Producer is !Sync).
        while let Ok(new_tap) = audio_tap_rx.try_recv() {
            tracing::info!(attached = new_tap.is_some(), "DSP: audio tap set");
            audio_tap = new_tap;
        }

        // 2. Pull `in_size` IQ samples from *each* consumer, in
        //    lockstep. Because all receivers ride the same wire packet,
        //    the rings stay approximately balanced, so the loop
        //    typically spins one micro-sleep or less per iteration.
        let mut all_full = false;
        while !all_full {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            all_full = true;
            for r in 0..num_rx {
                while gathered[r] < in_size {
                    match consumers[r].pop() {
                        Ok(sample) => {
                            wdsp_in[r][2 * gathered[r]]     = sample.i as f64;
                            wdsp_in[r][2 * gathered[r] + 1] = sample.q as f64;
                            gathered[r] += 1;
                        }
                        Err(_) => {
                            all_full = false;
                            break;
                        }
                    }
                }
            }
            if !all_full {
                thread::sleep(Duration::from_micros(500));
            }
        }
        gathered.fill(0);

        // 3. Run each channel's DSP pass.
        for r in 0..num_rx {
            if let Err(e) = channels[r].process(&mut wdsp_in[r], &mut wdsp_out[r]) {
                tracing::warn!(rx = r, error = %e, "WDSP process error");
                continue;
            }
        }
        samples_dsp.fetch_add(in_size as u64, Ordering::Relaxed);

        // 3b. Digital decoder tap.
        //     For any RX with an active DigitalPipeline, push the
        //     real part of the post-DSP audio (pre-volume, pre-mix)
        //     through the decoder. Decodes accumulate in
        //     `pending_decodes[r]` and are drained at telemetry time.
        for r in 0..num_rx {
            let Some(pipe) = digital[r].as_mut() else { continue };
            if !rx_state[r].enabled { continue; }
            digital_audio_buf.clear();
            digital_audio_buf.reserve(in_size);
            for n in 0..in_size {
                digital_audio_buf.push(wdsp_out[r][2 * n] as f32);
            }
            pipe.push_audio(&digital_audio_buf);
            pending_decodes[r].append(&mut pipe.drain_decodes());
        }

        // 4. Mix into a stereo audio burst.
        //
        //    - num_rx == 1:                  L = R = RX0
        //    - num_rx >= 2:                  L = RX0, R = RX1
        //    - disabled RX contributes 0     (enable flag gates output)
        //
        //    Any RX beyond index 1 is ignored for audio in phase B —
        //    they still get their spectrum published for the UI.
        let mut sum_sq: [f32; MAX_RX] = [0.0; MAX_RX];
        for n in 0..in_size {
            let l_sample = if rx_state[0].enabled {
                let s = (wdsp_out[0][2 * n] as f32) * rx_state[0].volume;
                sum_sq[0] += s * s;
                s
            } else {
                0.0
            };
            let r_sample = if num_rx >= 2 && rx_state[1].enabled {
                let s = (wdsp_out[1][2 * n] as f32) * rx_state[1].volume;
                sum_sq[1] += s * s;
                s
            } else if num_rx == 1 {
                // Mono case: duplicate L to R so stereo output still
                // plays through both speakers.
                l_sample
            } else {
                0.0
            };
            audio_burst[n] = [l_sample, r_sample];
        }
        // S-meter for any RX we don't mix into the stereo bus (index >= 2).
        for r in 2..num_rx {
            let mut ss = 0.0_f32;
            if rx_state[r].enabled {
                for n in 0..in_size {
                    let s = (wdsp_out[r][2 * n] as f32) * rx_state[r].volume;
                    ss += s * s;
                }
            }
            sum_sq[r] = ss;
        }

        let pushed = audio.push_stereo_slice(&audio_burst);
        if pushed < audio_burst.len() {
            tracing::trace!(
                dropped = audio_burst.len() - pushed,
                "audio ring full, dropping samples"
            );
        }

        // Mirror to the optional secondary tap (WebRTC). Non-blocking:
        // if the consumer side is slow or absent, we drop silently.
        if let Some(tap) = audio_tap.as_mut() {
            for frame in audio_burst.iter() {
                if tap.push(*frame).is_err() {
                    break;
                }
            }
        }

        // 5. Per-RX S-meter smoothing (200 ms time constant).
        for r in 0..num_rx {
            let rms = (sum_sq[r] / in_size as f32).sqrt();
            let raw_db = if rms > 1.0e-7 { 20.0 * rms.log10() } else { -140.0 };
            smoothed_s_meter_db[r] = 0.8 * smoothed_s_meter_db[r] + 0.2 * raw_db;
        }

        // 6. Spectrum publish — rate limited.
        if last_spectrum_push.elapsed() >= SPECTRUM_UPDATE_INTERVAL {
            let mut snapshot = Telemetry {
                num_rx: num_rx as u8,
                ..Telemetry::default()
            };
            for r in 0..num_rx {
                compute_spectrum(
                    &wdsp_in[r],
                    &hann,
                    fft_norm,
                    &fft,
                    &mut fft_buf,
                    &mut fft_scratch,
                );
                let bins_db = spectrum_to_db(&fft_buf);
                snapshot.rx[r] = RxTelemetry {
                    enabled:          rx_state[r].enabled,
                    spectrum_bins_db: bins_db,
                    s_meter_db:       smoothed_s_meter_db[r],
                    center_freq_hz:   center_freqs[r].load(Ordering::Acquire),
                    span_hz:          48_000,
                    mode:             rx_state[r].mode,
                    digital_mode:     rx_state[r].digital_mode,
                    digital_decodes:  std::mem::take(&mut pending_decodes[r]),
                    constellation:    digital[r]
                        .as_ref()
                        .map(|p| p.constellation())
                        .unwrap_or_default(),
                };
            }
            snapshot.last_update = Instant::now();
            telemetry.store(Arc::new(snapshot));
            last_spectrum_push = Instant::now();
        }
    }

    tracing::debug!("DSP loop exiting");
}

/// Fill `fft_buf` with a windowed copy of the interleaved input IQ
/// buffer (I at even indices, Q at odd) and run the forward FFT
/// in-place.
fn compute_spectrum(
    wdsp_in:  &[f64],
    hann:     &[f32],
    norm:     f32,
    fft:      &Arc<dyn Fft<f32>>,
    fft_buf:  &mut [Complex32],
    scratch:  &mut [Complex32],
) {
    debug_assert_eq!(wdsp_in.len(),    2 * fft_buf.len());
    debug_assert_eq!(hann.len(),       fft_buf.len());

    for (n, slot) in fft_buf.iter_mut().enumerate() {
        let i = wdsp_in[2 * n]     as f32;
        let q = wdsp_in[2 * n + 1] as f32;
        let w = hann[n] * norm;
        *slot = Complex32::new(i * w, q * w);
    }
    fft.process_with_scratch(fft_buf, scratch);
}

/// Convert FFT bins to log-magnitude dB, `fftshift`ed so bin 0 is the
/// lowest frequency (negative Nyquist) and bin N-1 the highest.
fn spectrum_to_db(fft_buf: &[Complex32]) -> Vec<f32> {
    let n    = fft_buf.len();
    let half = n / 2;
    let mut out = vec![0.0_f32; n];
    for (k, slot) in out.iter_mut().enumerate() {
        // rustfft outputs bins 0..N, where 0..N/2 are positive freqs
        // and N/2..N are negative freqs. Shift so negative comes first.
        let src = if k < half { k + half } else { k - half };
        let mag = fft_buf[src].norm();
        *slot = if mag > 1.0e-7 {
            20.0 * mag.log10()
        } else {
            -140.0
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpsdr_net::MockHl2;
    use std::time::Instant;

    fn init_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    }

    /// End-to-end smoke test against the loopback mock HL2. Verifies the
    /// three-stage pipeline (net → WDSP → audio) stays connected for a
    /// short while and actually moves bytes.
    ///
    /// This test **requires a working audio output device** — on CI it
    /// will skip itself. Local runs assert that samples flow through
    /// the DSP stage (easy to check) and, when the audio device is
    /// present, through the audio stage too.
    #[test]
    fn mock_roundtrip_pipes_samples_through_dsp() {
        init_tracing();

        let mock = MockHl2::spawn().expect("mock HL2");

        let mut cfg = RadioConfig {
            radio_addr:   mock.address(),
            num_rx:       1,
            audio_device: None,
            prime_wisdom: false, // skip the FFTW plan table rebuild (~10 min)
            ..RadioConfig::default()
        };
        cfg.rx[0] = RxConfig {
            enabled:      true,
            frequency_hz: 7_074_000,
            mode:         Mode::Usb,
            volume:       0.0, // silence — don't blast the test runner's speakers
        };

        // If there's no default output device, the Radio can't start.
        // Treat that as a skip.
        let radio = match Radio::start(cfg) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("no output device") || msg.contains("NoDevice") {
                    eprintln!("no audio device, skipping end-to-end test");
                    return;
                }
                panic!("Radio::start failed: {e:#}");
            }
        };

        // Let the pipeline run long enough that a full WDSP buffer
        // (1024 complex samples @ 48 kHz ≈ 21 ms) has definitely been
        // processed, with headroom for FFT plan creation on first call.
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            if radio.status().samples_dsp >= 1024 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let status = radio.status();
        assert!(
            status.samples_dsp >= 1024,
            "DSP thread didn't process any buffers: {status:?}"
        );
        assert!(
            status.session.packets_received > 0,
            "no packets received from mock: {status:?}"
        );

        radio.stop().expect("stop");
    }
}
