//! Benchmark `prime_wisdom_with_embedded_default` end-to-end.
//!
//! Usage:
//!   rm -rf ~/.cache/thetis/wdspWisdom00
//!   cargo run -p wdsp --example bench_wisdom --release
//!
//! Expected: < 1 s on a fresh cache (embedded blob is written, then
//! WDSPwisdom imports it). Without B.0.7 the same path took 1–10 min.

use std::time::Instant;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let t0 = Instant::now();
    match wdsp::prime_wisdom_with_embedded_default() {
        Ok(Some(status)) => println!("OK in {:?}: {:?}", t0.elapsed(), status),
        Ok(None) => println!("OK in {:?}: no cache dir", t0.elapsed()),
        Err(e) => println!("FAIL in {:?}: {e}", t0.elapsed()),
    }
}
