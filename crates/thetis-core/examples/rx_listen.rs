//! End-to-end RX probe for real hardware: spin up the full
//! network → WDSP → audio pipeline and listen.
//!
//! ```text
//! cargo run -p thetis-core --example rx_listen
//! HL2_IP=192.168.1.40 cargo run -p thetis-core --example rx_listen
//! HL2_IP=192.168.1.40 cargo run -p thetis-core --example rx_listen -- 14074000 USB 60
//! ```
//!
//! Args (all optional, must be in this order): `frequency_hz mode seconds`
//! where `mode` is one of `LSB|USB|AM|FM|CWU|CWL`.
//!
//! # Safety
//!
//! Same guarantees as `hpsdr-net`'s `rx_probe`: the pipeline is RX-only
//! by construction. No control frame is ever built with `mox = true` and
//! no TX-channel-type WDSP channel is opened. You can run this against
//! a radio with no antenna attached.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use thetis_core::{discover, DiscoveryOptions, Radio, RadioConfig, RxConfig, WdspMode};

const DEFAULT_SECONDS:   u64 = 30;
const DEFAULT_FREQUENCY: u32 = 7_074_000;
const DEFAULT_MODE:      WdspMode = WdspMode::Usb;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    print_banner();

    // Parse CLI args.
    let mut args = std::env::args().skip(1);
    let freq_hz: u32 = args
        .next()
        .map(|s| s.parse().expect("frequency_hz must be a positive integer"))
        .unwrap_or(DEFAULT_FREQUENCY);
    let mode: WdspMode = args
        .next()
        .map(|s| parse_mode(&s).expect("mode must be LSB/USB/AM/FM/CWU/CWL"))
        .unwrap_or(DEFAULT_MODE);
    let seconds: u64 = args
        .next()
        .map(|s| s.parse().expect("seconds must be a positive integer"))
        .unwrap_or(DEFAULT_SECONDS);

    // ---- Find the radio -----------------------------------------------
    let target_addr: SocketAddr = if let Ok(ip_str) = std::env::var("HL2_IP") {
        let ip: std::net::IpAddr = ip_str
            .parse()
            .map_err(|e| anyhow!("HL2_IP={ip_str:?} is not a valid IP: {e}"))?;
        SocketAddr::new(ip, hpsdr_net_port())
    } else {
        println!();
        println!("== Discovering radios ==");
        let radios = discover(&DiscoveryOptions {
            timeout: Duration::from_millis(750),
            targets: Vec::new(),
        })?;
        if radios.is_empty() {
            anyhow::bail!(
                "No radios found. Set HL2_IP=<ip> to skip discovery, e.g.\n\
                 \n    HL2_IP=192.168.1.40 cargo run -p thetis-core --example rx_listen\n"
            );
        }
        for (i, r) in radios.iter().enumerate() {
            println!(
                "  [{i}] {addr}  model={model:?}  busy={busy}",
                addr = r.addr, model = r.reply.model, busy = r.reply.busy,
            );
        }
        radios[0].addr
    };

    // ---- Start the radio -----------------------------------------------
    println!();
    println!(
        "== Starting RX on {addr} at {freq:.3} MHz, mode={mode:?}, duration={seconds}s ==",
        addr = target_addr,
        freq = freq_hz as f64 / 1.0e6,
    );

    let mut cfg = RadioConfig {
        radio_addr:   target_addr,
        num_rx:       1,
        audio_device: std::env::var("AUDIO_DEVICE").ok(),
        prime_wisdom: true,
        ..RadioConfig::default()
    };
    cfg.rx[0] = RxConfig {
        enabled:      true,
        frequency_hz: freq_hz,
        mode,
        volume:       0.25,
    };
    let radio = Radio::start(cfg)?;

    // ---- Ctrl+C handler -----------------------------------------------
    let keep_going = Arc::new(AtomicBool::new(true));
    {
        let flag = Arc::clone(&keep_going);
        ctrlc::set_handler(move || {
            eprintln!("\n(Ctrl+C received, stopping cleanly…)");
            flag.store(false, Ordering::Release);
        })?;
    }

    // ---- Stats every second -------------------------------------------
    let start    = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let mut next_tick = start + Duration::from_secs(1);
    let mut prev_pkts = 0u64;
    let mut prev_dsp  = 0u64;
    let mut prev_audio = 0u64;

    while keep_going.load(Ordering::Acquire) && Instant::now() < deadline {
        if Instant::now() >= next_tick {
            let s = radio.status();
            let pkts_delta  = s.session.packets_received - prev_pkts;
            let dsp_delta   = s.samples_dsp             - prev_dsp;
            let audio_delta = s.samples_audio           - prev_audio;
            prev_pkts  = s.session.packets_received;
            prev_dsp   = s.samples_dsp;
            prev_audio = s.samples_audio;

            println!(
                "t={:>3.0}s  pkts=+{:<4}  dsp=+{:<6}  audio=+{:<6}  underruns={}  seq_err={}",
                start.elapsed().as_secs_f64(),
                pkts_delta,
                dsp_delta,
                audio_delta,
                s.audio_underruns,
                s.session.sequence_errors,
            );
            next_tick += Duration::from_secs(1);
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    println!();
    println!("== Stopping ==");
    radio.stop()?;
    println!("== Done ==");
    Ok(())
}

fn print_banner() {
    println!("============================================================");
    println!("  rx_listen — full RX pipeline (HL2 → WDSP → audio)");
    println!("  • MOX never asserted, RX-only WDSP channel");
    println!("  • Safe to run with antenna disconnected or on a dummy load");
    println!("============================================================");
}

fn parse_mode(s: &str) -> Option<WdspMode> {
    match s.to_ascii_uppercase().as_str() {
        "LSB"  => Some(WdspMode::Lsb),
        "USB"  => Some(WdspMode::Usb),
        "AM"   => Some(WdspMode::Am),
        "SAM"  => Some(WdspMode::Sam),
        "FM"   => Some(WdspMode::Fm),
        "CWU"  => Some(WdspMode::CwU),
        "CWL"  => Some(WdspMode::CwL),
        "DIGU" => Some(WdspMode::DigU),
        "DIGL" => Some(WdspMode::DigL),
        _      => None,
    }
}

fn hpsdr_net_port() -> u16 {
    // Re-export dodge: the constant lives in `hpsdr-net`. We could add
    // it to `thetis-core`'s re-exports but it's such a trivial number
    // that the example just hardcodes the upstream value. If that ever
    // becomes a maintenance burden, plumb it through `thetis_core`.
    1024
}
