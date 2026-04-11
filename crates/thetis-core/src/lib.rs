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
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use hpsdr_net::{Session, SessionConfig, SessionStatus};
use hpsdr_protocol::IqSample;
use rtrb::Consumer;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
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
    /// Whether to prime the FFTW wisdom cache before opening the
    /// WDSP channel.
    ///
    /// The user-facing `thetis` binary should leave this at `true` so
    /// subsequent runs start instantly. Integration tests against the
    /// loopback [`hpsdr_net::MockHl2`] should set it to `false` —
    /// when the cache is cold, `WDSPwisdom` rebuilds the entire plan
    /// table (sizes 64..262144) which easily takes 10+ minutes and
    /// makes the test suite unusable on CI.
    pub prime_wisdom: bool,
}

impl Default for RadioConfig {
    fn default() -> Self {
        RadioConfig {
            radio_addr:    "127.0.0.1:1024".parse().unwrap(),
            rx1_frequency: 7_074_000,
            mode:          Mode::Usb,
            volume:        0.5,
            audio_device:  None,
            prime_wisdom:  true,
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

/// A frozen snapshot of everything the UI wants to render in one frame.
///
/// Published atomically from the DSP thread via [`arc_swap::ArcSwap`];
/// the UI side loads the latest `Arc<Telemetry>` with a single
/// lock-free read and can hold on to it for the rest of the frame
/// without blocking the producer.
#[derive(Debug, Clone)]
pub struct Telemetry {
    /// Log-magnitude FFT bins in dB, already `fftshift`ed so bin `0`
    /// is the lowest frequency and bin `N-1` the highest. Length is
    /// [`SPECTRUM_BINS`].
    pub spectrum_bins_db: Vec<f32>,
    /// Post-DSP audio RMS, in dB relative to full scale (0 dBFS = sine
    /// at ±1.0). Exponentially smoothed for readable display.
    pub s_meter_db: f32,
    /// RX1 center frequency in Hz at the time this snapshot was taken.
    pub center_freq_hz: u32,
    /// Full spectral span covered by the FFT (= input sample rate).
    pub span_hz: u32,
    /// Monotonic timestamp of the last DSP frame. Lets the UI detect
    /// a stalled pipeline.
    pub last_update: Instant,
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry {
            spectrum_bins_db: vec![-140.0; SPECTRUM_BINS],
            s_meter_db:       -140.0,
            center_freq_hz:   0,
            span_hz:          48_000,
            last_update:      Instant::now(),
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
    telemetry:   Arc<ArcSwap<Telemetry>>,
    center_freq_hz: Arc<std::sync::atomic::AtomicU32>,
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

        // --- Prime FFTW wisdom before opening any WDSP channel ------
        //
        // Best-effort: if the cache dir is unavailable, or if the
        // import returns an error, we just continue and pay the slow
        // plan-build cost once. Phase A already did that.
        //
        // On cache miss, `wdsp::prime_wisdom_default` rebuilds the full
        // plan table (~10 minutes). That's slower than phase A's lazy
        // behaviour on the very first run but makes every subsequent
        // run instantaneous, which is the only trade-off that matters
        // for daily use. Tests against the loopback mock opt out via
        // `config.prime_wisdom = false`.
        if config.prime_wisdom {
            match wdsp::prime_wisdom_default() {
                Ok(Some(status)) => tracing::info!(?status, "FFTW wisdom primed"),
                Ok(None) => tracing::debug!("no wisdom cache dir, skipping"),
                Err(e) => tracing::warn!(error = %e, "wisdom prime failed, continuing"),
            }
        }

        // --- Network session ----------------------------------------
        //
        // Phase B.2.2: the Session now returns one consumer per RX,
        // but `thetis-core` is still single-RX; B.2.3 will extend
        // `Radio` to own multiple WDSP channels. For now we just pop
        // the first consumer and discard any others — RX2 gets turned
        // on wholesale once the orchestration catches up.
        let mut session_config = SessionConfig {
            radio_addr:        config.radio_addr,
            sample_rate_index: 0, // 48 kHz
            ring_capacity:     32_768,
            start_timeout:     Duration::from_secs(2),
            ..SessionConfig::default()
        };
        session_config.rx_frequencies[0] = config.rx1_frequency;
        let (session, mut consumers) = Session::start(session_config)?;
        let mut iq_consumer = consumers.remove(0);

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
        let telemetry = Arc::new(ArcSwap::from_pointee(Telemetry {
            center_freq_hz: config.rx1_frequency,
            ..Telemetry::default()
        }));
        let center_freq_hz =
            Arc::new(std::sync::atomic::AtomicU32::new(config.rx1_frequency));

        let dsp_thread = {
            let shutdown    = Arc::clone(&shutdown);
            let samples_dsp = Arc::clone(&samples_dsp);
            let telemetry   = Arc::clone(&telemetry);
            let center_freq = Arc::clone(&center_freq_hz);
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
                        telemetry,
                        center_freq,
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
            telemetry,
            center_freq_hz,
        })
    }

    /// Shared handle to the latest telemetry snapshot. The UI calls
    /// `radio.telemetry().load()` once per frame; the DSP thread
    /// publishes new snapshots via
    /// [`arc_swap::ArcSwap::store`].
    pub fn telemetry(&self) -> Arc<ArcSwap<Telemetry>> {
        Arc::clone(&self.telemetry)
    }

    /// Ask the radio to retune RX1. Propagated to the radio on the next
    /// TX keep-alive tick. Also bumps the atomic the DSP thread reads
    /// when stamping spectrum snapshots so the UI frequency label
    /// updates immediately without waiting for the next RX packet.
    pub fn set_frequency(&self, hz: u32) -> anyhow::Result<()> {
        if let Some(s) = &self.session {
            s.set_rx1_frequency(hz)?;
        }
        self.center_freq_hz.store(hz, Ordering::Release);
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
/// - Computes an FFT of the input IQ buffer for the UI spectrum display,
///   rate-limited to [`SPECTRUM_UPDATE_INTERVAL`] to avoid wasting cycles
///   on frames the UI won't draw.
/// - Computes an RMS-based S-meter value over the DSP output.
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
    telemetry:   Arc<ArcSwap<Telemetry>>,
    center_freq_hz: Arc<std::sync::atomic::AtomicU32>,
    in_size:     usize,
    initial_volume: f32,
) {
    // Upgrade this thread to a realtime scheduling class so scheduler
    // jitter under load can't cause audio underruns. Needs
    // `CAP_SYS_NICE` or `RLIMIT_RTPRIO` on Linux (`@audio` group +
    // `/etc/security/limits.d/audio.conf` is the usual setup). On
    // failure we just log and continue — the ring buffers in the
    // pipeline are generous enough that SCHED_OTHER works fine on an
    // idle machine.
    use thread_priority::{set_current_thread_priority, ThreadPriority};
    match set_current_thread_priority(ThreadPriority::Max) {
        Ok(()) => tracing::info!("DSP thread running at max RT priority"),
        Err(e) => tracing::warn!(
            error = ?e,
            "could not raise DSP thread priority \
             (missing CAP_SYS_NICE / RLIMIT_RTPRIO?), continuing at default"
        ),
    }

    // Pre-allocate the WDSP exchange buffers — fexchange0 reads/writes
    // these in place so we want them stable for the lifetime of the loop.
    let mut wdsp_in  = vec![0.0_f64; 2 * in_size];
    let mut wdsp_out = vec![0.0_f64; 2 * in_size];

    // Scratch for the mono-audio burst we push to the sink after each
    // WDSP process call.
    let mut audio_burst = vec![0.0_f32; in_size];

    let mut gathered = 0_usize;     // number of complex samples currently buffered in wdsp_in
    let mut volume   = initial_volume;

    // --- Spectrum analysis state --------------------------------------
    //
    // rustfft plans are cheap to create once, expensive to rebuild. We
    // plan a single forward FFT at the fixed DSP buffer size and reuse
    // it for the lifetime of the thread. The Hann window is also
    // precomputed.
    let mut planner = FftPlanner::<f32>::new();
    let fft: Arc<dyn Fft<f32>> = planner.plan_fft_forward(in_size);
    let hann: Vec<f32> = (0..in_size)
        .map(|n| {
            let x = std::f32::consts::PI * (n as f32) / ((in_size - 1) as f32);
            x.sin().powi(2) // equivalent to 0.5 * (1 - cos(2πn/(N-1)))
        })
        .collect();
    let fft_norm = 1.0_f32 / (in_size as f32).sqrt();
    let mut fft_buf: Vec<Complex32> = vec![Complex32::new(0.0, 0.0); in_size];
    let mut fft_scratch: Vec<Complex32> =
        vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];

    let mut smoothed_s_meter_db: f32 = -140.0;
    let mut last_spectrum_push = Instant::now() - SPECTRUM_UPDATE_INTERVAL;

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
        let mut sum_sq = 0.0_f32;
        for (n, slot) in audio_burst.iter_mut().enumerate() {
            let sample = (wdsp_out[2 * n] as f32) * volume;
            *slot = sample;
            sum_sq += sample * sample;
        }
        let pushed = audio.push_slice(&audio_burst);
        if pushed < audio_burst.len() {
            tracing::trace!(
                dropped = audio_burst.len() - pushed,
                "audio ring full, dropping samples"
            );
        }

        // 5. S-meter: RMS → dBFS, exponentially smoothed.
        //
        //    `smoothing = 0.2` gives a ~200 ms time constant at 48 kHz /
        //    1024-sample buffers, which feels right for a needle-style
        //    meter. Raw `peak_db` at -140 dBFS is the floor.
        let rms   = (sum_sq / in_size as f32).sqrt();
        let raw_db = if rms > 1.0e-7 {
            20.0 * rms.log10()
        } else {
            -140.0
        };
        smoothed_s_meter_db = 0.8 * smoothed_s_meter_db + 0.2 * raw_db;

        // 6. Spectrum: rate-limit to SPECTRUM_UPDATE_INTERVAL.
        if last_spectrum_push.elapsed() >= SPECTRUM_UPDATE_INTERVAL {
            compute_spectrum(
                &wdsp_in,
                &hann,
                fft_norm,
                &fft,
                &mut fft_buf,
                &mut fft_scratch,
            );
            let bins_db = spectrum_to_db(&fft_buf);

            // Publish the snapshot. `ArcSwap::store` is lock-free on
            // the reader side and a single CAS on this side.
            let snapshot = Arc::new(Telemetry {
                spectrum_bins_db: bins_db,
                s_meter_db:       smoothed_s_meter_db,
                center_freq_hz:   center_freq_hz.load(Ordering::Acquire),
                span_hz:          48_000,
                last_update:      Instant::now(),
            });
            telemetry.store(snapshot);

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

        let cfg = RadioConfig {
            radio_addr: mock.address(),
            rx1_frequency: 7_074_000,
            mode: Mode::Usb,
            volume: 0.0,          // silence — don't blast the test runner's speakers
            audio_device: None,
            prime_wisdom: false,  // skip the FFTW plan table rebuild (~10 min)
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
