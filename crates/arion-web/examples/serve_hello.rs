//! Standalone smoke test: serves the SPA with a synthetic telemetry
//! source (animated spectrum + S-meter) and a standalone `App` so
//! user actions round-trip without a real radio.
//!
//! `cargo run -p arion-web --example serve_hello`
//! Binds on 0.0.0.0:8080 so the page is reachable from any device on
//! the LAN (phone, tablet).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use arion_app::{App, AppOptions};
use arion_core::{RxTelemetry, Telemetry, SPECTRUM_BINS};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = Arc::new(Mutex::new(App::new(AppOptions::default())));
    let telemetry = Arc::new(ArcSwap::new(Arc::new(Telemetry::default())));
    // No audio tap in the demo — the WebRTC peer falls back to a
    // synthetic 440 Hz tone so the plumbing is testable.
    let audio_tap = Arc::new(Mutex::new(None));

    let app_for_synth = app.clone();
    let tel_for_synth = telemetry.clone();
    thread::spawn(move || synth_loop(app_for_synth, tel_for_synth));

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    arion_web::serve_blocking(addr, app, telemetry, audio_tap)
}

fn synth_loop(app: Arc<Mutex<App>>, telemetry: Arc<ArcSwap<Telemetry>>) {
    let start = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(43));
        let t = start.elapsed().as_secs_f32();
        let (num_rx, freq0, vol0) = {
            let a = app.lock().unwrap();
            (
                a.num_rx(),
                a.rxs().first().map(|r| r.frequency_hz).unwrap_or(0),
                a.rxs().first().map(|r| r.volume).unwrap_or(0.25),
            )
        };
        let mut snap = Telemetry {
            num_rx,
            last_update: Instant::now(),
            ..Telemetry::default()
        };
        snap.rx[0] = RxTelemetry {
            enabled:          true,
            center_freq_hz:   freq0,
            spectrum_bins_db: synth_bins(t, vol0),
            s_meter_db:       -90.0 + 30.0 * (t * 0.7).sin(),
            span_hz:          48_000,
            mode:             snap.rx[0].mode,
        };
        telemetry.store(Arc::new(snap));
    }
}

fn synth_bins(t: f32, gain: f32) -> Vec<f32> {
    let mut v = Vec::with_capacity(SPECTRUM_BINS);
    let g_db = (gain.max(1e-3)).log10() * 20.0;
    for i in 0..SPECTRUM_BINS {
        let x = i as f32 / SPECTRUM_BINS as f32;
        let noise = -120.0 + 8.0 * ((x * 50.0 + t).sin() * 0.5 + 0.5);
        // two drifting carriers
        let c1 = 0.25 + 0.1 * (t * 0.15).sin();
        let c2 = 0.65 + 0.08 * (t * 0.22).cos();
        let peak = |pos: f32, width: f32, amp: f32| {
            let d = (x - pos) / width;
            amp * (-d * d).exp()
        };
        let s = noise + peak(c1, 0.012, 45.0) + peak(c2, 0.006, 55.0);
        v.push(s + g_db);
    }
    v
}
