//! Cross-platform audio output for Thetis.
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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, StreamConfig};

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
    /// Total mono samples the callback has consumed from the ring (i.e.
    /// how much DSP audio has actually reached the speakers). Counts
    /// mono samples *before* the L/R fan-out, so 48 kHz of playback
    /// increments this by 48 000 per second.
    pub samples_played: AtomicU64,

    /// Number of callback invocations that ran out of data and had to
    /// fill part of the output buffer with silence. Small numbers right
    /// after start are normal; if this keeps climbing during steady-state
    /// playback the DSP thread is too slow.
    pub underruns: AtomicU64,
}

/// Producer-side handle passed to the DSP thread.
pub struct AudioSink {
    producer: rtrb::Producer<f32>,
    stats:    Arc<AudioStats>,
}

impl AudioSink {
    /// Push one mono sample. Returns `false` if the ring is full (i.e.
    /// the audio callback hasn't caught up yet — almost always a sign
    /// that the DSP is running ahead of real time, which shouldn't
    /// happen once everything is in sync).
    pub fn push(&mut self, sample: f32) -> bool {
        self.producer.push(sample).is_ok()
    }

    /// Push as many of `samples` as fit. Returns the number actually
    /// pushed; any tail that didn't fit is dropped on the floor.
    pub fn push_slice(&mut self, samples: &[f32]) -> usize {
        let mut n = 0;
        for s in samples {
            if self.producer.push(*s).is_err() {
                break;
            }
            n += 1;
        }
        n
    }

    /// How many more samples will fit without blocking.
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
pub struct AudioOutput {
    _stream: cpal::Stream,
    stats:   Arc<AudioStats>,
    device_name: String,
    channels:    u16,
    sample_rate: u32,
}

impl AudioOutput {
    /// Open an output device and start the stream.
    ///
    /// Returns the [`AudioOutput`] handle (holds the cpal stream alive)
    /// and an [`AudioSink`] for the DSP thread to feed.
    pub fn start(config: AudioConfig) -> Result<(Self, AudioSink), AudioError> {
        let host = cpal::default_host();

        let device = match config.device_name.as_deref() {
            None => host.default_output_device().ok_or(AudioError::NoDevice)?,
            Some(name) => find_output_device(&host, name)?,
        };
        let device_name = device.name().unwrap_or_else(|_| "<unnamed>".into());
        tracing::info!(device = %device_name, "opening audio output device");

        // Pick a stream config. Phase A: we require f32 / `config.sample_rate`.
        // We accept whatever channel count the device advertises and fan
        // mono samples across all of them in the callback.
        let supported = device
            .supported_output_configs()?
            .filter(|c| c.sample_format() == SampleFormat::F32)
            .filter(|c| {
                c.min_sample_rate().0 <= config.sample_rate
                    && config.sample_rate <= c.max_sample_rate().0
            })
            .max_by_key(|c| c.channels())
            .ok_or_else(|| AudioError::UnsupportedFormat {
                device:      device_name.clone(),
                sample_rate: config.sample_rate,
                channels:    2,
            })?;

        let channels    = supported.channels();
        let sample_rate = config.sample_rate;
        let stream_config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: BufferSize::Default,
        };
        tracing::info!(
            channels,
            sample_rate,
            "cpal stream config chosen"
        );

        // Ring buffer: DSP producer → callback consumer.
        let (producer, mut consumer) =
            rtrb::RingBuffer::<f32>::new(config.ring_capacity);

        let stats = Arc::new(AudioStats::default());
        let stats_cb = Arc::clone(&stats);

        // Build the output stream. Error callback logs and drops — we
        // don't try to recover in phase A because a mid-session failure
        // usually means the user unplugged headphones.
        let err_fn = |e| tracing::error!(error = %e, "cpal stream error");

        let stream = device.build_output_stream(
            &stream_config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                output_callback(out, channels as usize, &mut consumer, &stats_cb);
            },
            err_fn,
            None, // no timeout
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
                sample_rate,
            },
            sink,
        ))
    }

    pub fn stats(&self) -> &AudioStats  { &self.stats }
    pub fn device_name(&self) -> &str    { &self.device_name }
    pub fn channels(&self) -> u16        { self.channels }
    pub fn sample_rate(&self) -> u32     { self.sample_rate }
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

/// cpal output callback. Pops mono samples from the ring and writes them
/// to every channel of the interleaved output buffer. When the ring is
/// empty, fills the remaining frames with silence and bumps the underrun
/// counter so the DSP thread has a visible signal that it's too slow.
fn output_callback(
    out:      &mut [f32],
    channels: usize,
    consumer: &mut rtrb::Consumer<f32>,
    stats:    &AudioStats,
) {
    let mut frame_idx     = 0;
    let mut frames_played = 0u64;
    let mut underrun      = false;

    while frame_idx + channels <= out.len() {
        match consumer.pop() {
            Ok(sample) => {
                for ch in 0..channels {
                    out[frame_idx + ch] = sample;
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

        // Push 512 samples of silence and wait for the callback to
        // consume them.
        for _ in 0..512 {
            sink.push(0.0);
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
