//! Headless Arion: connects to the HL2 and serves the browser
//! frontend. No egui window, no TUI — just the radio plus HTTP
//! + WebSocket + WebRTC on the configured address.
//!
//! Env vars:
//! - `HL2_IP` — radio IP (overrides saved setting).
//! - `ARION_WEB_LISTEN` — bind address, defaults to `0.0.0.0:8080`.

use std::net::SocketAddr;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use arion_app::{App, AppOptions};
use arion_core::StereoFrame;
use arion_web::{Action, StateSnapshot};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let opts = AppOptions {
        radio_ip_override: std::env::var("HL2_IP").ok(),
    };
    let app = Arc::new(Mutex::new(App::new(opts)));

    // Connect to the radio right away.
    {
        let mut a = app.lock().unwrap();
        a.connect();
        if !a.is_connected() {
            let err = a
                .last_error()
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".into());
            return Err(anyhow!("radio failed to connect: {err}"));
        }
    }

    // Grab the telemetry arc and attach an audio tap.
    let (telemetry, audio_tap) = {
        let a = app.lock().unwrap();
        let radio = a
            .radio()
            .ok_or_else(|| anyhow!("radio handle unexpectedly missing"))?;
        let telemetry = a
            .telemetry()
            .ok_or_else(|| anyhow!("telemetry unexpectedly missing"))?
            .clone();

        let (producer, consumer) = rtrb::RingBuffer::<StereoFrame>::new(48_000 / 2);
        radio.set_audio_tap(Some(producer))?;
        let tap: arion_web::SharedAudioTap = Arc::new(Mutex::new(Some(consumer)));
        (telemetry, tap)
    };

    // Snapshot publisher — updates the web view state at ~10 Hz.
    let snapshot: arion_web::SharedSnapshot =
        Arc::new(ArcSwap::new(Arc::new(StateSnapshot::default())));
    {
        let app = app.clone();
        let tel = telemetry.clone();
        let snap = snapshot.clone();
        thread::Builder::new()
            .name("web-snapshot".into())
            .spawn(move || loop {
                thread::sleep(Duration::from_millis(100));
                let a = app.lock().unwrap_or_else(|p| p.into_inner());
                let t = tel.load_full();
                let s = StateSnapshot::from_app_and_telemetry(&a, &t);
                snap.store(Arc::new(s));
            })?;
    }

    // Action drain — applies browser-dispatched actions to `App`.
    let (action_tx, action_rx) = mpsc::channel::<Action>();
    {
        let app = app.clone();
        thread::Builder::new()
            .name("web-actions".into())
            .spawn(move || {
                for action in action_rx {
                    let mut a = app.lock().unwrap_or_else(|p| p.into_inner());
                    action.apply(&mut a);
                }
            })?;
    }

    // Per-frame persistence tick — App::tick normally runs on the
    // egui frame timer. Without it, debounced saves never fire.
    {
        let app = app.clone();
        thread::Builder::new()
            .name("app-tick".into())
            .spawn(move || loop {
                thread::sleep(Duration::from_millis(200));
                let mut a = app.lock().unwrap_or_else(|p| p.into_inner());
                a.tick(std::time::Instant::now());
            })?;
    }

    let addr: SocketAddr = std::env::var("ARION_WEB_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()?;
    tracing::info!(%addr, "arion-web headless starting");
    arion_web::serve_blocking(addr, snapshot, action_tx, telemetry, audio_tap)
}
