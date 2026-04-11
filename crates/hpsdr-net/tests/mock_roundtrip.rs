//! End-to-end test: spin up a [`MockHl2`], discover it via unicast to
//! loopback, open a session, verify that samples flow, then stop cleanly.
//!
//! Run with `cargo test -p hpsdr-net`.

use std::time::{Duration, Instant};

use hpsdr_net::{discover, DiscoveryOptions, MockHl2, Session, SessionConfig};
use hpsdr_protocol::discovery::HpsdrModel;

fn init_tracing() {
    // Best-effort: don't fail the test if another test already set a
    // global subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

#[test]
fn discover_finds_mock_and_reports_hermeslite() {
    init_tracing();
    let mock = MockHl2::spawn().expect("spawn mock HL2");

    let opts = DiscoveryOptions {
        timeout: Duration::from_millis(500),
        targets: vec![mock.address()],
    };
    let radios = discover(&opts).expect("discover");

    assert_eq!(radios.len(), 1, "expected exactly one mock radio");
    let info = radios[0];
    assert_eq!(info.addr, mock.address());
    assert_eq!(info.reply.model, HpsdrModel::HermesLite);
    assert!(!info.reply.busy);
    assert_eq!(info.reply.num_rxs, Some(1));
}

#[test]
fn session_start_receives_samples_and_stops_cleanly() {
    init_tracing();
    let mock = MockHl2::spawn().expect("spawn mock HL2");

    let config = SessionConfig {
        radio_addr: mock.address(),
        rx1_frequency: 7_074_000,
        sample_rate_index: 0,
        ring_capacity: 8_192,
        start_timeout: Duration::from_secs(2),
    };

    let (session, mut consumer) = Session::start(config).expect("session start");

    // Collect samples for up to 250 ms. The mock emits roughly
    // 1000 packets/sec × 126 samples = ~126 k samples/sec, so we expect
    // the ring to fill fast.
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut collected = 0usize;
    while Instant::now() < deadline && collected < 2_000 {
        if let Ok(_sample) = consumer.pop() {
            collected += 1;
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    assert!(
        collected >= 2_000,
        "expected to drain at least 2000 samples in 250 ms, got {collected}"
    );

    let status = session.status();
    assert!(status.running);
    assert!(status.packets_received > 0);
    assert!(status.is_connected(Instant::now()));

    session.stop().expect("stop");
}

#[test]
fn session_reports_disconnection_after_mock_drop() {
    init_tracing();
    let mock = MockHl2::spawn().expect("spawn mock HL2");

    let config = SessionConfig {
        radio_addr: mock.address(),
        start_timeout: Duration::from_secs(2),
        ..SessionConfig::default()
    };
    let (session, mut consumer) = Session::start(config).expect("session start");

    // Burn through a handful of samples so we know the link was alive.
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        if consumer.pop().is_err() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(session.status().packets_received > 0);

    // Pull the plug.
    drop(mock);

    // Wait longer than the 1-second "connected" threshold and confirm the
    // watchdog flips.
    std::thread::sleep(Duration::from_millis(1_200));
    let status = session.status();
    assert!(!status.is_connected(Instant::now()));
}
