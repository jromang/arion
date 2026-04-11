//! Receive-only smoke probe for a real HPSDR Protocol 1 radio.
//!
//! ```text
//! cargo run -p hpsdr-net --example rx_probe -- [frequency_hz] [seconds]
//! ```
//!
//! What it does:
//! 1. Broadcasts a discovery request and lists every radio it hears back.
//! 2. Opens a `Session` against the first radio found.
//! 3. Streams samples for the requested number of seconds (default 10),
//!    printing a stats line every second.
//! 4. Sends the Stop command cleanly on exit (Ctrl+C or duration elapsed).
//!
//! # SAFETY NOTICE
//!
//! This example **never** asserts MOX/PTT. The `hpsdr-net` session code
//! builds every control frame with `mox = false` and only writes register
//! 0 (config) and register 2 (RX1 NCO). Nothing here keys the transmitter.
//! You can safely run it with your antenna disconnected or a dummy load.
//!
//! That said, *no software is a substitute for physical precautions*. The
//! recommended workflow on first contact with real hardware is:
//! - Antenna connector terminated in a dummy load (or nothing at all).
//! - PA drive at minimum in the radio's firmware.
//! - Hand mic / footswitch unplugged.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hpsdr_net::{discover, DiscoveryOptions, RadioInfo, Session, SessionConfig, HPSDR_PORT};
use hpsdr_protocol::discovery::{DiscoveryReply, HpsdrModel};
use rtrb::Consumer;

fn main() -> anyhow::Result<()> {
    // Make tracing output visible — users pass `RUST_LOG=debug` if they
    // want the gory details.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    print_safety_banner();

    // Parse optional CLI args: frequency (Hz) and run duration (seconds).
    let mut args = std::env::args().skip(1);
    let freq_hz: u32 = args
        .next()
        .map(|s| s.parse().expect("frequency_hz must be a positive integer"))
        .unwrap_or(7_074_000);
    let seconds: u64 = args
        .next()
        .map(|s| s.parse().expect("seconds must be a positive integer"))
        .unwrap_or(10);

    // ---- Target selection ---------------------------------------------
    //
    // Two modes:
    // - `HL2_IP=<ipv4>` in the environment: skip discovery and go straight
    //   to the radio. Convenient when UFW / a weird interface layout /
    //   corporate VPN makes broadcast unreliable.
    // - Otherwise: broadcast discovery and pick the first non-busy radio.
    let target = if let Ok(ip_str) = std::env::var("HL2_IP") {
        let ip: IpAddr = ip_str.parse().map_err(|e| {
            anyhow::anyhow!("HL2_IP={ip_str:?} is not a valid IP address: {e}")
        })?;
        let addr = SocketAddr::new(ip, HPSDR_PORT);
        println!();
        println!("== HL2_IP override: skipping discovery, targeting {addr} ==");

        // Still do a directed unicast discovery so we can sanity-check the
        // radio is answering and print its real model / firmware version.
        let radios = discover(&DiscoveryOptions {
            timeout: Duration::from_millis(750),
            targets: vec![addr],
        })?;
        if radios.is_empty() {
            anyhow::bail!(
                "No reply from {addr}. Is the radio powered on and on this subnet? \
                 Also double-check that UDP port 1024 is reachable \
                 (try `sudo ufw allow from {ip} to any port 1024 proto udp`)."
            );
        }
        report_radios(&radios);
        radios[0]
    } else {
        println!();
        println!("== Discovering radios on the local network ==");
        let radios = discover(&DiscoveryOptions {
            timeout: Duration::from_millis(750),
            targets: Vec::new(), // empty => broadcast
        })?;

        if radios.is_empty() {
            anyhow::bail!(
                "No radios found via broadcast. If you know the radio's IP, run:\n\
                 \n    HL2_IP=192.168.1.xx cargo run -p hpsdr-net --example rx_probe\n\
                 \n\
                 If you have a firewall enabled, it may be dropping the reply — \
                 try temporarily disabling it or allowing UDP port 1024."
            );
        }
        report_radios(&radios);

        let t = radios[0];
        if t.reply.busy {
            anyhow::bail!(
                "Radio {} is reporting busy — another client has it open. \
                 Close the other session before running this probe.",
                t.addr
            );
        }
        t
    };
    let _: DiscoveryReply = target.reply; // type hint for the reader

    // ---- Session start -------------------------------------------------
    println!();
    println!(
        "== Starting RX session against {addr} at {freq:.3} MHz ==",
        addr = target.addr,
        freq = freq_hz as f64 / 1.0e6,
    );

    let config = SessionConfig {
        radio_addr:        target.addr,
        rx1_frequency:     freq_hz,
        sample_rate_index: 0, // 48 kHz
        ring_capacity:     32_768,
        start_timeout:     Duration::from_secs(2),
    };
    let (session, consumer) = Session::start(config)?;

    // ---- Ctrl+C handler ------------------------------------------------
    let keep_going = Arc::new(AtomicBool::new(true));
    {
        let flag = Arc::clone(&keep_going);
        ctrlc::set_handler(move || {
            eprintln!("\n(Ctrl+C received, stopping cleanly…)");
            flag.store(false, Ordering::Release);
        })?;
    }

    // ---- Stream loop with per-second stats -----------------------------
    let stats_result = run_stream_loop(&session, consumer, seconds, &keep_going);

    // ---- Clean shutdown regardless of how we got here ------------------
    println!();
    println!("== Stopping session ==");
    session.stop()?;
    println!("== Done ==");

    stats_result
}

fn print_safety_banner() {
    println!("============================================================");
    println!("  rx_probe — RX-only smoke test for real HPSDR P1 hardware");
    println!("  • MOX is never asserted");
    println!("  • Only register 0 (config) and register 2 (RX1 NCO) are");
    println!("    written");
    println!("  • Run with antenna disconnected or on a dummy load the");
    println!("    first time");
    println!("============================================================");
}

fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn report_radios(radios: &[RadioInfo]) {
    for (i, r) in radios.iter().enumerate() {
        println!(
            "  [{i}] {addr}  mac={mac}  model={model:?}  fw={fw}  busy={busy}",
            addr = r.addr,
            mac = format_mac(&r.reply.mac),
            model = r.reply.model,
            fw = r.reply.code_version,
            busy = r.reply.busy,
        );
    }
    // Mention HL2 discovery explicitly for the operator's benefit.
    if radios.iter().any(|r| r.reply.model == HpsdrModel::HermesLite) {
        println!("  (HermesLite 2 detected — good to go)");
    }
}

fn run_stream_loop(
    session: &Session,
    mut consumer: Consumer<hpsdr_protocol::IqSample>,
    seconds: u64,
    keep_going: &AtomicBool,
) -> anyhow::Result<()> {
    let start    = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let mut next_tick = start + Duration::from_secs(1);

    // Previous counters so we can compute per-second deltas.
    let mut prev_packets = 0u64;
    let mut prev_samples = 0u64;

    // Drained-sample peak tracking: rolling RMS of `i` over the last
    // second, just to give the operator a visual heartbeat.
    let mut acc_i2: f64 = 0.0;
    let mut acc_count: u64 = 0;

    while keep_going.load(Ordering::Acquire) && Instant::now() < deadline {
        // Drain the ring.
        while let Ok(sample) = consumer.pop() {
            acc_i2 += (sample.i as f64) * (sample.i as f64);
            acc_count += 1;
        }

        // Once a second: status line.
        if Instant::now() >= next_tick {
            let status       = session.status();
            let pkt_delta    = status.packets_received - prev_packets;
            let sample_delta = status.samples_received - prev_samples;
            prev_packets     = status.packets_received;
            prev_samples     = status.samples_received;

            let rms = if acc_count > 0 {
                (acc_i2 / acc_count as f64).sqrt()
            } else {
                0.0
            };
            acc_i2 = 0.0;
            acc_count = 0;

            println!(
                "t={:>3.0}s  pkts=+{:<5}  samples=+{:<7}  seq_err={:<4}  rms(i)={:>7.5}  connected={}",
                start.elapsed().as_secs_f64(),
                pkt_delta,
                sample_delta,
                status.sequence_errors,
                rms,
                status.is_connected(Instant::now()),
            );
            next_tick += Duration::from_secs(1);
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    Ok(())
}

