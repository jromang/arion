//! Smoke-test the audio output without involving the DSP pipeline.
//!
//! ```text
//! cargo run -p thetis-audio --example audio_probe
//! ```
//!
//! Plays a 440 Hz sine for two seconds (at very low level) through the
//! default output device. If you hear a clean tone, the cpal backend
//! and `thetis-audio::AudioOutput` are both wired correctly and we can
//! move on to [`thetis-core`], which glues the radio → DSP → audio
//! pipeline together.
//!
//! Environment knobs:
//! - `AUDIO_DEVICE=<name>` — pick a non-default device. Run with
//!   `cargo run -p thetis-audio --example audio_probe -- --list` to see
//!   the available names.

use std::f32::consts::TAU;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use thetis_audio::{list_output_devices, AudioConfig, AudioOutput};

const SAMPLE_RATE: u32 = 48_000;
const TONE_HZ:     f32 = 440.0;
const DURATION:    Duration = Duration::from_secs(2);
/// -30 dBFS — quiet enough not to startle anyone, loud enough to hear
/// even on a laptop speaker.
const AMPLITUDE:   f32 = 0.03;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // `--list` dumps every output device cpal can see and exits.
    if std::env::args().any(|a| a == "--list") {
        println!("Output devices:");
        for (name, is_default) in list_output_devices()? {
            let marker = if is_default { " (default)" } else { "" };
            println!("  - {name}{marker}");
        }
        return Ok(());
    }

    let config = AudioConfig {
        device_name:   std::env::var("AUDIO_DEVICE").ok(),
        sample_rate:   SAMPLE_RATE,
        ring_capacity: 8_192,
    };
    let (out, mut sink) = AudioOutput::start(config)?;

    println!();
    println!("Device:      {}", out.device_name());
    println!("Sample rate: {} Hz", out.sample_rate());
    println!("Channels:    {}", out.channels());
    println!("Tone:        {TONE_HZ:.0} Hz, {DURATION:?}, amplitude {AMPLITUDE}");
    println!();
    println!("Play a quiet sine through the default output. Listen for ~2 s of");
    println!("clean tone; silence or stutter means the ring is underrunning or");
    println!("cpal isn't happy with the stream config.");
    println!();

    // Generator: 440 Hz sine at 48 kHz. Use a running phase so samples
    // stay continuous across push batches (no clicks at boundaries).
    let mut phase: f32 = 0.0;
    let phase_step   = TAU * TONE_HZ / SAMPLE_RATE as f32;

    let start    = Instant::now();
    let deadline = start + DURATION;
    while Instant::now() < deadline {
        // Push as many samples as fit in the ring right now, then yield.
        let free = sink.free_capacity();
        if free == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let mut batch = Vec::with_capacity(free);
        for _ in 0..free {
            batch.push(AMPLITUDE * phase.sin());
            phase += phase_step;
            if phase > TAU {
                phase -= TAU;
            }
        }
        let pushed = sink.push_slice(&batch);
        if pushed < batch.len() {
            // Shouldn't happen — `free` should always equal what the ring accepts.
            eprintln!("warning: only pushed {pushed} of {} samples", batch.len());
        }
    }

    // Give the last buffered samples time to reach the speakers.
    std::thread::sleep(Duration::from_millis(150));

    let stats = out.stats();
    println!();
    println!(
        "samples_played={}  underruns={}",
        stats.samples_played.load(Ordering::Relaxed),
        stats.underruns.load(Ordering::Relaxed),
    );
    Ok(())
}
