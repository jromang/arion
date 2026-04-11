//! One-shot tool to generate a FFTW wisdom blob from the vendored
//! `wdsp-sys` FFTW build, for baking into the app via `include_bytes!`.
//!
//! Usage:
//!     cargo run -p wdsp --example gen_wisdom --release -- <out_path>
//!
//! It removes any existing wisdom at the default cache dir, calls
//! `WDSPwisdom` to rebuild from scratch (takes 1-3 minutes in release),
//! and copies the resulting `wdspWisdom00` to `<out_path>`. The result
//! is committed under `crates/wdsp/data/wdspWisdom00` and embedded into
//! every build so fresh installs don't pay the rebuild cost.

use std::env;
use std::fs;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let Some(out_path) = env::args().nth(1) else {
        eprintln!("usage: gen_wisdom <out_path>");
        return ExitCode::from(2);
    };

    let Some(cache_dir) = wdsp::wisdom::default_cache_dir() else {
        eprintln!("no cache directory available on this platform");
        return ExitCode::from(1);
    };
    let cache_file = cache_dir.join("wdspWisdom00");

    // Force a rebuild by removing any stale cache file.
    if cache_file.exists() {
        fs::remove_file(&cache_file).unwrap_or_else(|e| {
            eprintln!("failed to remove existing wisdom cache {cache_file:?}: {e}");
            std::process::exit(1);
        });
    }

    println!("Priming FFTW wisdom in {cache_dir:?} — this will take 1-3 minutes.");
    let t0 = Instant::now();
    match wdsp::wisdom::prime(&cache_dir) {
        Ok(status) => println!("wisdom prime done in {:?}: {:?}", t0.elapsed(), status),
        Err(e) => {
            eprintln!("wisdom prime failed: {e}");
            return ExitCode::from(1);
        }
    }

    let len = fs::metadata(&cache_file)
        .map(|m| m.len())
        .unwrap_or_else(|e| {
            eprintln!("wisdom cache file missing after prime: {e}");
            std::process::exit(1);
        });
    println!("wisdom cache file: {cache_file:?} ({len} bytes)");

    fs::copy(&cache_file, &out_path).unwrap_or_else(|e| {
        eprintln!("failed to copy wisdom to {out_path}: {e}");
        std::process::exit(1);
    });
    println!("wrote {out_path} ({len} bytes)");

    ExitCode::SUCCESS
}
