//! Cross-platform audio output for Arion.
//!
//! This crate wraps [`cpal`] with a very narrow API aimed at one job:
//! take mono `f32` audio samples pushed from the DSP thread and play them
//! out on the default (or user-selected) audio device at 48 kHz, with
//! enough ring-buffer headroom to tolerate brief DSP stalls without
//! underrunning.
//!
//! # Architecture
//!
//! ```text
//!   DSP thread                 cpal output callback
//!       │ push_slice(samples)           │
//!       ▼                               ▼
//!   Producer<f32> ──── rtrb ring ────► Consumer<f32>
//!                                      │  dup mono → L+R
//!                                      ▼
//!                                    speakers
//! ```
//!
//! The DSP side owns an [`AudioSink`] (the `rtrb::Producer`). The audio
//! callback runs inside `cpal`'s driver thread and owns the `Consumer`
//! plus a reference to [`AudioStats`] for counters.
//!
//! # Phase A constraints
//!
//! - **48 kHz only.** If the default device can't do 48 kHz in `f32`,
//!   [`AudioOutput::start`] fails. Phase B brings in `rubato` so we can
//!   resample between whatever the device prefers and the DSP rate.
//! - **Mono DSP, fanned out to every output channel.** The callback
//!   replicates each mono sample across every channel cpal gives us
//!   (mono → L/R stereo, or 1→6 for a surround device).
//! - **No TX path.** Input audio (mic → transmitter) is a phase B story.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SupportedStreamConfigRange, StreamConfig};
use rubato::{FftFixedIn, Resampler};

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no output device available")]
    NoDevice,

    #[error("device \"{0}\" not found")]
    DeviceNotFound(String),

    #[error(
        "device \"{device}\" does not support {sample_rate} Hz / f32 / {channels}ch \
         (phase A requires this exact format — phase B will add resampling)"
    )]
    UnsupportedFormat {
        device: String,
        sample_rate: u32,
        channels: u16,
    },

    #[error("cpal error: {0}")]
    Cpal(String),
}

impl From<cpal::BuildStreamError> for AudioError {
    fn from(e: cpal::BuildStreamError) -> Self { AudioError::Cpal(e.to_string()) }
}
impl From<cpal::PlayStreamError> for AudioError {
    fn from(e: cpal::PlayStreamError) -> Self { AudioError::Cpal(e.to_string()) }
}
impl From<cpal::DefaultStreamConfigError> for AudioError {
    fn from(e: cpal::DefaultStreamConfigError) -> Self { AudioError::Cpal(e.to_string()) }
}
impl From<cpal::SupportedStreamConfigsError> for AudioError {
    fn from(e: cpal::SupportedStreamConfigsError) -> Self { AudioError::Cpal(e.to_string()) }
}
impl From<cpal::DevicesError> for AudioError {
    fn from(e: cpal::DevicesError) -> Self { AudioError::Cpal(e.to_string()) }
}

/// List the names of all available output audio devices on the
/// default host. Returns an empty `Vec` (not an error) if the host
/// has no output devices — the UI displays "(no devices)" instead.
pub fn enumerate_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Knobs for [`AudioOutput::start`].
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Name of the device to open. `None` picks the system default.
    pub device_name: Option<String>,
    /// Sample rate the DSP is producing. Phase A always sets this to
    /// `48_000` and expects the device to match.
    pub sample_rate: u32,
    /// Capacity of the ring buffer in mono samples. 16 k ≈ 340 ms at
    /// 48 kHz, enough to ride through any hiccup on a well-behaved host.
    pub ring_capacity: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            device_name:   None,
            sample_rate:   48_000,
            ring_capacity: 16_384,
        }
    }
}

/// Counters maintained by the cpal callback. All fields are atomic so
/// they can be read from any thread without locking.
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Total stereo **frames** the callback has consumed from the ring
    /// (a frame is one `(L, R)` pair). So 48 kHz of playback increments
    /// this by 48 000 per second.
    pub samples_played: AtomicU64,

    /// Number of callback invocations that ran out of data and had to
    /// fill part of the output buffer with silence. Small numbers right
    /// after start are normal; if this keeps climbing during steady-state
    /// playback the DSP thread is too slow.
    pub underruns: AtomicU64,
}

/// One stereo frame: `[L, R]`. Multi-RX mixes RX1 into L and RX2 into R
/// — if there's only one receiver, both channels carry the same mono
/// sample.
pub type StereoFrame = [f32; 2];

/// Producer-side handle passed to the DSP thread.
///
/// The underlying ring stores [`StereoFrame`]s so each push is atomic
/// at the frame level — the cpal callback can never see a torn
/// half-frame.
pub struct AudioSink {
    producer: rtrb::Producer<StereoFrame>,
    stats:    Arc<AudioStats>,
}

impl AudioSink {
    /// Push one stereo frame. Returns `false` if the ring is full.
    pub fn push_stereo(&mut self, l: f32, r: f32) -> bool {
        self.producer.push([l, r]).is_ok()
    }

    /// Push as many stereo frames as fit. Returns the number actually
    /// pushed; any tail that didn't fit is dropped on the floor.
    pub fn push_stereo_slice(&mut self, frames: &[StereoFrame]) -> usize {
        let mut n = 0;
        for &frame in frames {
            if self.producer.push(frame).is_err() {
                break;
            }
            n += 1;
        }
        n
    }

    /// Back-compat: push a mono sample by duplicating it to both
    /// channels. Useful for single-RX consumers that don't want to
    /// think about stereo.
    pub fn push_mono(&mut self, sample: f32) -> bool {
        self.push_stereo(sample, sample)
    }

    /// How many more stereo frames will fit without blocking.
    pub fn free_capacity(&self) -> usize {
        self.producer.slots()
    }

    /// Read-only view of the playback counters. Shared with the audio
    /// callback, so values update in real time.
    pub fn stats(&self) -> &AudioStats {
        &self.stats
    }
}

/// A running audio output. Drop to stop playback and release the device.
///
/// When the output device can't run at the DSP rate directly, a helper
/// thread owned by this handle bridges the two rates through a
/// `rubato::FftFixedIn` resampler. Dropping the handle stops that
/// thread (via `shutdown`), then drops the cpal stream.
pub struct AudioOutput {
    _stream: cpal::Stream,
    stats:   Arc<AudioStats>,
    device_name: String,
    channels:    u16,
    /// Rate the cpal stream is actually running at (may differ from
    /// the DSP rate requested in [`AudioConfig`]).
    device_rate: u32,
    /// Rate the DSP feeds `AudioSink::push_slice` at.
    dsp_rate:    u32,

    // Resampler bridge (only present when device_rate != dsp_rate).
    resampler_shutdown: Option<Arc<AtomicBool>>,
    resampler_thread:   Option<JoinHandle<()>>,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        if let Some(flag) = self.resampler_shutdown.as_ref() {
            flag.store(true, Ordering::Release);
        }
        if let Some(handle) = self.resampler_thread.take() {
            let _ = handle.join();
        }
        // cpal::Stream drops last, shutting the device down.
    }
}

impl AudioOutput {
    /// Open an output device and start the stream.
    ///
    /// Picks the best-matching cpal stream config on the requested
    /// device. If the device can run exactly at `config.sample_rate`
    /// (the DSP rate), the pipeline is direct: DSP producer → rtrb →
    /// cpal callback. Otherwise a helper thread bridges DSP-rate
    /// samples to the device's actual rate through a
    /// `rubato::FftFixedIn` resampler.
    ///
    /// Returns the [`AudioOutput`] handle (holds the cpal stream +
    /// resampler thread alive) and an [`AudioSink`] for the DSP thread
    /// to feed at the DSP rate.
    pub fn start(config: AudioConfig) -> Result<(Self, AudioSink), AudioError> {
        let host = cpal::default_host();

        let device = match config.device_name.as_deref() {
            None => host.default_output_device().ok_or(AudioError::NoDevice)?,
            Some(name) => find_output_device(&host, name)?,
        };
        let device_name = device.name().unwrap_or_else(|_| "<unnamed>".into());
        tracing::info!(device = %device_name, "opening audio output device");

        // --- Pick a stream config --------------------------------------
        //
        // Preference order:
        //   1. An f32 config whose sample-rate range includes the DSP
        //      rate exactly. No resampling needed, simplest pipeline.
        //   2. An f32 config at any other rate. A `rubato::FftFixedIn`
        //      thread will bridge DSP rate → device rate.
        //
        // Within each tier we pick the one with the most channels so
        // multi-channel devices (surround, pro audio interfaces) can
        // fan the mono signal out to every speaker.
        let supported: Vec<SupportedStreamConfigRange> = device
            .supported_output_configs()?
            .filter(|c| c.sample_format() == SampleFormat::F32)
            .collect();
        if supported.is_empty() {
            return Err(AudioError::UnsupportedFormat {
                device:      device_name.clone(),
                sample_rate: config.sample_rate,
                channels:    2,
            });
        }

        let dsp_rate = config.sample_rate;
        let (stream_config, device_rate) = pick_stream_config(&supported, dsp_rate)
            .ok_or_else(|| AudioError::UnsupportedFormat {
                device:      device_name.clone(),
                sample_rate: dsp_rate,
                channels:    2,
            })?;
        let channels = stream_config.channels;
        let need_resampling = device_rate != dsp_rate;
        tracing::info!(
            channels,
            dsp_rate,
            device_rate,
            need_resampling,
            "cpal stream config chosen"
        );

        let stats = Arc::new(AudioStats::default());
        let err_fn = |e| tracing::error!(error = %e, "cpal stream error");

        // --- Build the pipeline depending on whether we need resampling.
        if need_resampling {
            // Outer ring: DSP pushes stereo frames at dsp_rate.
            let (outer_producer, outer_consumer) =
                rtrb::RingBuffer::<StereoFrame>::new(config.ring_capacity);

            // Inner ring: resampler thread writes stereo frames at
            // device_rate, cpal callback reads.
            let inner_capacity = (config.ring_capacity as f64 * device_rate as f64
                                    / dsp_rate as f64)
                .ceil() as usize
                + 2048;
            let (inner_producer, mut inner_consumer) =
                rtrb::RingBuffer::<StereoFrame>::new(inner_capacity);

            let resampler_shutdown = Arc::new(AtomicBool::new(false));
            let resampler_thread = {
                let shutdown = Arc::clone(&resampler_shutdown);
                thread::Builder::new()
                    .name("arion-audio-resample".into())
                    .spawn(move || {
                        resample_bridge(
                            outer_consumer,
                            inner_producer,
                            dsp_rate,
                            device_rate,
                            shutdown,
                        )
                    })
                    .map_err(|e| AudioError::Cpal(e.to_string()))?
            };

            let stats_cb = Arc::clone(&stats);
            let stream = device.build_output_stream(
                &stream_config,
                move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    output_callback(
                        out,
                        channels as usize,
                        &mut inner_consumer,
                        &stats_cb,
                    );
                },
                err_fn,
                None,
            )?;
            stream.play()?;

            let sink = AudioSink {
                producer: outer_producer,
                stats:    Arc::clone(&stats),
            };
            Ok((
                AudioOutput {
                    _stream: stream,
                    stats,
                    device_name,
                    channels,
                    device_rate,
                    dsp_rate,
                    resampler_shutdown: Some(resampler_shutdown),
                    resampler_thread:   Some(resampler_thread),
                },
                sink,
            ))
        } else {
            // Fast path: direct DSP → cpal ring, no resampler thread.
            let (producer, mut consumer) =
                rtrb::RingBuffer::<StereoFrame>::new(config.ring_capacity);

            let stats_cb = Arc::clone(&stats);
            let stream = device.build_output_stream(
                &stream_config,
                move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                    output_callback(out, channels as usize, &mut consumer, &stats_cb);
                },
                err_fn,
                None,
            )?;
            stream.play()?;

            let sink = AudioSink {
                producer,
                stats: Arc::clone(&stats),
            };
            Ok((
                AudioOutput {
                    _stream: stream,
                    stats,
                    device_name,
                    channels,
                    device_rate,
                    dsp_rate,
                    resampler_shutdown: None,
                    resampler_thread:   None,
                },
                sink,
            ))
        }
    }

    pub fn stats(&self) -> &AudioStats  { &self.stats }
    pub fn device_name(&self) -> &str    { &self.device_name }
    pub fn channels(&self) -> u16        { self.channels }
    /// Rate at which the cpal stream actually runs on the hardware.
    /// Equal to [`Self::dsp_rate`] when no resampling is needed.
    pub fn device_rate(&self) -> u32     { self.device_rate }
    /// Rate the DSP thread feeds via [`AudioSink::push_slice`].
    pub fn dsp_rate(&self) -> u32        { self.dsp_rate }
    /// Back-compat alias for [`Self::device_rate`].
    pub fn sample_rate(&self) -> u32     { self.device_rate }
}

/// Pick the best [`SupportedStreamConfigRange`] for our DSP rate.
///
/// Returns `(StreamConfig, chosen_rate)`. `chosen_rate == dsp_rate` when
/// we found an exact match (no resampling), otherwise it's whatever
/// rate the device prefers in its closest supported range.
fn pick_stream_config(
    supported: &[SupportedStreamConfigRange],
    dsp_rate: u32,
) -> Option<(StreamConfig, u32)> {
    // Tier 1: exact-rate match (no resampling).
    let exact = supported
        .iter()
        .filter(|c| {
            c.min_sample_rate().0 <= dsp_rate && dsp_rate <= c.max_sample_rate().0
        })
        .max_by_key(|c| c.channels());
    if let Some(c) = exact {
        return Some((
            StreamConfig {
                channels:    c.channels(),
                sample_rate: cpal::SampleRate(dsp_rate),
                buffer_size: BufferSize::Default,
            },
            dsp_rate,
        ));
    }

    // Tier 2: no exact match — pick the range whose max_sample_rate is
    // closest to our DSP rate (distance = |max_rate - dsp_rate|), then
    // use that range's max rate.
    let best = supported.iter().min_by_key(|c| {
        (c.max_sample_rate().0 as i64 - dsp_rate as i64).abs()
    })?;
    let chosen_rate = best.max_sample_rate().0;
    Some((
        StreamConfig {
            channels:    best.channels(),
            sample_rate: cpal::SampleRate(chosen_rate),
            buffer_size: BufferSize::Default,
        },
        chosen_rate,
    ))
}

/// Resampler bridge thread. Pulls DSP-rate stereo frames out of
/// `outer_consumer`, de-interleaves them into separate L and R
/// buffers, runs them through a single 2-channel
/// `rubato::FftFixedIn`, re-interleaves the output, and pushes the
/// device-rate frames into `inner_producer` for the cpal callback to
/// drain.
///
/// Chunk size: 1024 DSP frames (~21 ms @ 48 kHz), matching the DSP
/// thread's `fexchange0` buffer. Most of the time the resampler
/// consumes and produces exactly one chunk per wake-up.
fn resample_bridge(
    mut outer_consumer: rtrb::Consumer<StereoFrame>,
    mut inner_producer: rtrb::Producer<StereoFrame>,
    dsp_rate:  u32,
    device_rate: u32,
    shutdown:  Arc<AtomicBool>,
) {
    const CHUNK_IN_FRAMES: usize = 1024;

    let mut resampler = match FftFixedIn::<f32>::new(
        dsp_rate as usize,
        device_rate as usize,
        CHUNK_IN_FRAMES,
        1, // 1 sub-chunk is fine for our latency budget
        2, // stereo: two rubato "channels"
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to create resampler");
            return;
        }
    };

    // Pre-allocated de-interleaved buffers so the loop never touches
    // the allocator.
    let mut input_l:  Vec<f32> = vec![0.0; CHUNK_IN_FRAMES];
    let mut input_r:  Vec<f32> = vec![0.0; CHUNK_IN_FRAMES];
    let mut output_chunks: Vec<Vec<f32>> = resampler.output_buffer_allocate(true);

    while !shutdown.load(Ordering::Acquire) {
        // Gather one full input chunk of stereo frames.
        let mut filled = 0;
        while filled < CHUNK_IN_FRAMES {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            match outer_consumer.pop() {
                Ok([l, r]) => {
                    input_l[filled] = l;
                    input_r[filled] = r;
                    filled += 1;
                }
                Err(_) => thread::sleep(Duration::from_micros(500)),
            }
        }

        // Process one chunk — rubato takes two separate channel
        // slices and writes into two separate output slices.
        let input_slices: [&[f32]; 2] = [&input_l[..], &input_r[..]];
        let (left_out, right_out) = output_chunks.split_at_mut(1);
        let mut output_slices: [&mut [f32]; 2] =
            [&mut left_out[0][..], &mut right_out[0][..]];
        let (_in_frames, out_frames) = match resampler.process_into_buffer(
            &input_slices,
            &mut output_slices,
            None,
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "resampler process error");
                continue;
            }
        };

        // Re-interleave and push to the inner ring.
        let out_l = &output_chunks[0];
        let out_r = &output_chunks[1];
        for n in 0..out_frames {
            let frame = [out_l[n], out_r[n]];
            if inner_producer.push(frame).is_err() {
                // Inner ring full — cpal callback is behind or the
                // device hasn't drained yet. Back off briefly and
                // keep trying so we don't lose data.
                while !shutdown.load(Ordering::Acquire)
                    && inner_producer.push(frame).is_err()
                {
                    thread::sleep(Duration::from_micros(500));
                }
            }
        }
    }
}

/// List every output device cpal can see, as `(name, is_default)` pairs.
pub fn list_output_devices() -> Result<Vec<(String, bool)>, AudioError> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    let mut out = Vec::new();
    for dev in host.output_devices()? {
        let name = dev.name().unwrap_or_else(|_| "<unnamed>".into());
        let is_default = Some(&name) == default_name.as_ref();
        out.push((name, is_default));
    }
    Ok(out)
}

fn find_output_device(host: &cpal::Host, name: &str) -> Result<Device, AudioError> {
    for dev in host.output_devices()? {
        if dev.name().ok().as_deref() == Some(name) {
            return Ok(dev);
        }
    }
    Err(AudioError::DeviceNotFound(name.into()))
}

/// cpal output callback. Pops stereo frames from the ring and writes
/// them to the device output, handling the full spectrum of channel
/// counts cpal might hand us:
///
/// - `channels == 1` → sum of (L + R) × 0.5 on the lone channel
/// - `channels == 2` → L on ch0, R on ch1 (the typical stereo case)
/// - `channels >= 3` → L on ch0, R on ch1, silence on every remaining
///   channel. We don't try to be clever about surround — phase B keeps
///   the mapping predictable.
///
/// On underrun (ring empty) the rest of the output buffer is filled
/// with silence and the underrun counter is incremented once per
/// callback invocation that ran short.
fn output_callback(
    out:      &mut [f32],
    channels: usize,
    consumer: &mut rtrb::Consumer<StereoFrame>,
    stats:    &AudioStats,
) {
    let mut frame_idx     = 0;
    let mut frames_played = 0u64;
    let mut underrun      = false;

    while frame_idx + channels <= out.len() {
        match consumer.pop() {
            Ok([l, r]) => {
                match channels {
                    1 => {
                        out[frame_idx] = (l + r) * 0.5;
                    }
                    2 => {
                        out[frame_idx]     = l;
                        out[frame_idx + 1] = r;
                    }
                    _ => {
                        out[frame_idx]     = l;
                        out[frame_idx + 1] = r;
                        for x in &mut out[frame_idx + 2..frame_idx + channels] {
                            *x = 0.0;
                        }
                    }
                }
                frames_played += 1;
            }
            Err(_) => {
                underrun = true;
                for x in &mut out[frame_idx..] {
                    *x = 0.0;
                }
                break;
            }
        }
        frame_idx += channels;
    }

    if frames_played > 0 {
        stats.samples_played.fetch_add(frames_played, Ordering::Relaxed);
    }
    if underrun {
        stats.underruns.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit test: push a buffer through the resampler bridge in
    /// isolation (no cpal involved) and confirm it produces
    /// approximately the expected number of output samples with no
    /// NaN / infinity and no panic. 48 kHz → 44.1 kHz is the
    /// round-number case that trips most naive resamplers.
    #[test]
    fn resampler_bridge_48_to_44100_produces_expected_frame_count() {
        use std::f32::consts::TAU;

        const DSP_RATE:    u32 = 48_000;
        const DEVICE_RATE: u32 = 44_100;
        const INPUT_FRAMES: usize = 8 * 1024; // 8 chunks = ~170 ms

        // Producer side: one chunk of 1 kHz sine for 170 ms, stereo
        // (RX1 → L, RX2 → R with a 90° phase shift to be
        // distinguishable from L on decode).
        let (mut outer_producer, outer_consumer) =
            rtrb::RingBuffer::<StereoFrame>::new(INPUT_FRAMES * 2);
        for n in 0..INPUT_FRAMES {
            let phase = TAU * 1_000.0 * n as f32 / DSP_RATE as f32;
            let l = phase.sin()       * 0.5;
            let r = (phase + 1.57).sin() * 0.25;
            outer_producer.push([l, r]).unwrap();
        }

        let inner_capacity = INPUT_FRAMES * 2;
        let (inner_producer, mut inner_consumer) =
            rtrb::RingBuffer::<StereoFrame>::new(inner_capacity);

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_inner = Arc::clone(&shutdown);

        let handle = std::thread::spawn(move || {
            resample_bridge(
                outer_consumer,
                inner_producer,
                DSP_RATE,
                DEVICE_RATE,
                shutdown_inner,
            )
        });

        // Expected count ≈ INPUT_FRAMES * 44100 / 48000 ≈ 7527.
        // FftFixedIn has internal latency — the first chunk or two
        // are buffered before output starts flowing, so the realised
        // count is a bit below this ideal. We accept 70% of theoretical
        // as "the pipeline is producing usable output"; the real
        // quality check is that every sample is finite and bounded.
        let ideal = (INPUT_FRAMES as f64 * DEVICE_RATE as f64 / DSP_RATE as f64)
            .floor() as usize;
        let minimum = ideal * 7 / 10;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut drained: Vec<StereoFrame> = Vec::with_capacity(ideal);
        while drained.len() < ideal
            && std::time::Instant::now() < deadline
        {
            if let Ok(frame) = inner_consumer.pop() {
                drained.push(frame);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        shutdown.store(true, Ordering::Release);
        let _ = handle.join();

        assert!(
            drained.len() >= minimum,
            "expected at least {minimum} resampled frames (ideal {ideal}), got {}",
            drained.len()
        );
        for (i, &[l, r]) in drained.iter().enumerate() {
            assert!(l.is_finite() && r.is_finite(), "frame {i} = [{l}, {r}] NaN/Inf");
            assert!(l.abs() <= 1.0 && r.abs() <= 1.0,
                "frame {i} = [{l}, {r}] out of range");
        }
    }

    /// Smoke test: on machines with a working audio device, open the
    /// default output, push some silence, and verify the callback runs
    /// at least once. Skipped automatically when there is no device
    /// (typical CI container). Not ignored by default so a local run
    /// catches cpal regressions.
    #[test]
    fn output_device_plays_a_short_buffer_of_silence() {
        let host = cpal::default_host();
        if host.default_output_device().is_none() {
            eprintln!("no output device available, skipping");
            return;
        }

        let config = AudioConfig {
            sample_rate:   48_000,
            ring_capacity: 4_096,
            ..AudioConfig::default()
        };
        let (out, mut sink) = match AudioOutput::start(config) {
            Ok(x)  => x,
            Err(AudioError::UnsupportedFormat { .. }) => {
                eprintln!("device does not support 48 kHz f32, skipping");
                return;
            }
            Err(e) => panic!("failed to start audio: {e}"),
        };

        // Push 512 stereo frames of silence and wait for the callback
        // to consume them.
        for _ in 0..512 {
            sink.push_stereo(0.0, 0.0);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        while std::time::Instant::now() < deadline {
            if out.stats().samples_played.load(Ordering::Relaxed) > 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "callback never ran; samples_played=0 after 250 ms (device may have rejected the config)"
        );
    }
}
