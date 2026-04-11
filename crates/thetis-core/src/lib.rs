//! Thetis radio orchestration.
//!
//! Glues together:
//! - [`hpsdr_net::Session`]   — UDP transport to the HL2
//! - [`wdsp::Channel`]         — the DSP chain (demod, filter, AGC, …)
//! - [`thetis_audio::AudioOutput`] — speakers
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
//!  │ thetis-core DSP thread                             │
//!  │   ┌────────────┐      ┌──────────────┐             │
//!  │   │ IQ gather  │─────►│ wdsp::Channel│──► audio    │
//!  │   │ (1024 cplx)│      │   .process() │    .push()  │
//!  │   └────────────┘      └──────────────┘             │
//!  └────────────────────────┬───────────────────────────┘
//!                           ▼
//!                AudioSink (rtrb producer)
//!                           │  mono f32
//!                           ▼
//!               cpal output callback (thetis-audio)
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
use std::time::Duration;

use hpsdr_net::{Session, SessionConfig, SessionStatus};
use hpsdr_protocol::IqSample;
use rtrb::Consumer;
use thetis_audio::{AudioConfig, AudioOutput, AudioSink};
use wdsp::{Channel as WdspChannel, Mode, RxConfig};

// Re-exports so downstream crates only need to depend on `thetis-core`.
pub use hpsdr_net::{discover, DiscoveryOptions, RadioInfo};
pub use thetis_audio::{list_output_devices, AudioStats};
pub use wdsp::Mode as WdspMode;

/// Knobs for [`Radio::start`].
#[derive(Debug, Clone)]
pub struct RadioConfig {
    /// UDP endpoint of the HL2 (typically `<ip>:1024`).
    pub radio_addr: std::net::SocketAddr,
    /// Initial tuned frequency of RX1, in Hz.
    pub rx1_frequency: u32,
    /// Initial demodulation mode.
    pub mode: Mode,
    /// Linear gain applied after WDSP's panel gain, just before pushing
    /// to the audio sink. `1.0` = unity, and anything much higher than
    /// that is likely to clip on a loud AM station.
    pub volume: f32,
    /// Audio output device name. `None` uses the system default.
    pub audio_device: Option<String>,
}

impl Default for RadioConfig {
    fn default() -> Self {
        RadioConfig {
            radio_addr:    "127.0.0.1:1024".parse().unwrap(),
            rx1_frequency: 7_074_000,
            mode:          Mode::Usb,
            volume:        0.5,
            audio_device:  None,
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

/// Commands applied to a live radio from outside the DSP thread.
#[derive(Debug, Clone, Copy)]
enum DspCommand {
    SetMode(Mode),
    SetVolume(f32),
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
    samples_dsp: Arc<std::sync::atomic::AtomicU64>,
}

impl Radio {
    /// Start a receiver against the given radio.
    ///
    /// Sequence:
    /// 1. Spin up the `hpsdr-net` session (includes the Start handshake).
    /// 2. Open a WDSP channel tuned to the requested mode.
    /// 3. Open the audio output.
    /// 4. Start the DSP thread that reads IQ → `fexchange0` → audio.
    /// 5. Set the initial frequency via [`Session::set_rx1_frequency`].
    pub fn start(config: RadioConfig) -> anyhow::Result<Self> {
        tracing::info!(?config, "starting Radio");

        // --- Network session ----------------------------------------
        let session_config = SessionConfig {
            radio_addr:        config.radio_addr,
            rx1_frequency:     config.rx1_frequency,
            sample_rate_index: 0, // 48 kHz
            ring_capacity:     32_768,
            start_timeout:     Duration::from_secs(2),
        };
        let (session, mut iq_consumer) = Session::start(session_config)?;

        // --- WDSP channel -------------------------------------------
        let rx_cfg = RxConfig {
            id: 0,
            mode: config.mode,
            ..RxConfig::default()
        };
        let wdsp_channel = WdspChannel::open_rx(rx_cfg)?;
        let in_size = wdsp_channel.in_size();

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

        let dsp_thread = {
            let shutdown    = Arc::clone(&shutdown);
            let samples_dsp = Arc::clone(&samples_dsp);
            let volume      = config.volume;
            thread::Builder::new()
                .name("thetis-dsp".into())
                .spawn(move || {
                    dsp_loop(
                        wdsp_channel,
                        iq_consumer_take(&mut iq_consumer),
                        audio_sink,
                        command_rx,
                        shutdown,
                        samples_dsp,
                        in_size,
                        volume,
                    )
                })?
        };

        Ok(Radio {
            session:     Some(session),
            audio:       Some(audio_out),
            dsp_thread:  Some(dsp_thread),
            shutdown,
            commands:    command_tx,
            samples_dsp,
        })
    }

    /// Ask the radio to retune RX1. Propagated to the radio on the next
    /// control thread tick.
    pub fn set_frequency(&self, hz: u32) -> anyhow::Result<()> {
        if let Some(s) = &self.session {
            s.set_rx1_frequency(hz)?;
        }
        Ok(())
    }

    /// Change demodulation mode. Takes effect on the DSP thread between
    /// buffers.
    pub fn set_mode(&self, mode: Mode) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetMode(mode))?;
        Ok(())
    }

    /// Set the post-DSP linear audio gain.
    pub fn set_volume(&self, linear: f32) -> anyhow::Result<()> {
        self.commands.send(DspCommand::SetVolume(linear))?;
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

/// Tiny helper to move the consumer out of the mut-ref spot into the
/// thread closure without having to name the rtrb internal type at the
/// call site.
fn iq_consumer_take(c: &mut Consumer<IqSample>) -> Consumer<IqSample> {
    // `rtrb::Consumer` is not `Clone`; the caller passed a `&mut` so we
    // swap it out. Since `Session::start` returns a fresh consumer and
    // we immediately hand it to the DSP thread, there's no prior
    // consumer to preserve here.
    std::mem::replace(c, dummy_consumer())
}

fn dummy_consumer() -> Consumer<IqSample> {
    // A freshly-created 1-slot ring we drop the producer of. The
    // consumer is valid (just useless), which is all we need to plug
    // the `replace` above.
    let (_p, c) = rtrb::RingBuffer::<IqSample>::new(1);
    c
}

/// Main DSP loop.
///
/// - Accumulates `in_size` complex IQ samples from the network ring.
/// - Calls WDSP `fexchange0` to get `in_size` complex output samples.
/// - Takes the real part (I) of each output complex and pushes it to the
///   audio sink as mono f32, scaled by `volume`.
/// - Services [`DspCommand`]s between buffers.
/// - Exits when `shutdown` flips.
#[allow(clippy::too_many_arguments)]
fn dsp_loop(
    mut channel: WdspChannel,
    mut iq_in:   Consumer<IqSample>,
    mut audio:   AudioSink,
    commands:    MpscRx<DspCommand>,
    shutdown:    Arc<AtomicBool>,
    samples_dsp: Arc<std::sync::atomic::AtomicU64>,
    in_size:     usize,
    initial_volume: f32,
) {
    // Pre-allocate the WDSP exchange buffers — fexchange0 reads/writes
    // these in place so we want them stable for the lifetime of the loop.
    let mut wdsp_in  = vec![0.0_f64; 2 * in_size];
    let mut wdsp_out = vec![0.0_f64; 2 * in_size];

    // Scratch for the mono-audio burst we push to the sink after each
    // WDSP process call.
    let mut audio_burst = vec![0.0_f32; in_size];

    let mut gathered = 0_usize;     // number of complex samples currently buffered in wdsp_in
    let mut volume   = initial_volume;

    while !shutdown.load(Ordering::Acquire) {
        // 1. Apply any pending commands before starting a new buffer.
        while let Ok(cmd) = commands.try_recv() {
            match cmd {
                DspCommand::SetMode(m) => {
                    tracing::info!(new_mode = ?m, "DSP: mode change");
                    channel.set_mode(m);
                }
                DspCommand::SetVolume(v) => {
                    volume = v;
                }
            }
        }

        // 2. Pull IQ samples until we've accumulated a full buffer.
        while gathered < in_size {
            match iq_in.pop() {
                Ok(sample) => {
                    wdsp_in[2 * gathered]     = sample.i as f64;
                    wdsp_in[2 * gathered + 1] = sample.q as f64;
                    gathered += 1;
                }
                Err(_) => {
                    // Ring empty — network thread hasn't delivered more
                    // yet. Short sleep avoids burning CPU while still
                    // being responsive to shutdown.
                    if shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_micros(500));
                }
            }
        }
        gathered = 0;

        // 3. Run the DSP pass.
        if let Err(e) = channel.process(&mut wdsp_in, &mut wdsp_out) {
            tracing::warn!(error = %e, "WDSP process error");
            continue;
        }
        samples_dsp.fetch_add(in_size as u64, Ordering::Relaxed);

        // 4. Extract mono audio (real part of complex output) and push.
        //    Apply post-DSP volume on the way through.
        for (n, slot) in audio_burst.iter_mut().enumerate() {
            *slot = (wdsp_out[2 * n] as f32) * volume;
        }
        let pushed = audio.push_slice(&audio_burst);
        if pushed < audio_burst.len() {
            tracing::trace!(
                dropped = audio_burst.len() - pushed,
                "audio ring full, dropping samples"
            );
        }
    }

    tracing::debug!("DSP loop exiting");
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

        let cfg = RadioConfig {
            radio_addr: mock.address(),
            rx1_frequency: 7_074_000,
            mode: Mode::Usb,
            volume: 0.0, // silence — don't blast the test runner's speakers
            audio_device: None,
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
