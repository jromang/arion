use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/wsprd");
    println!("cargo:rerun-if-changed=vendor/wsprd");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=shim.c");

    let mut build = cc::Build::new();
    build
        .include(&vendor)
        .flag_if_supported("-std=c99")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-Wno-unused-variable")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-result")
        .flag_if_supported("-Wno-unused-value")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-implicit-function-declaration")
        .flag_if_supported("-Wno-parentheses")
        .flag_if_supported("-Wno-pointer-sign")
        .flag_if_supported("-Wno-format-overflow")
        .flag_if_supported("-Wno-format-truncation")
        .flag_if_supported("-Wno-stringop-truncation");

    // The FFTW-free subset: Fano decoder, metric tables, callsign
    // hash, and unpk_. wsprd.c itself (file I/O + spectral search)
    // is skipped — Rust side does that with rustfft. wsprsim / gran
    // / jelinek aren't needed for the decode-only path.
    for f in [
        "fano.c",
        "tab.c",
        "mettab.c",
        "metric_tables.c",
        "nhash.c",
        "wsprd_utils.c",
        "wsprsim_utils.c", // get_wspr_channel_symbols: text → 162-tone message
        "gran.c",          // Gaussian noise (called from wsprsim_utils)
    ] {
        build.file(vendor.join(f));
    }
    build.file(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shim.c"));
    build.compile("wsprd");
    println!("cargo:rustc-link-lib=m");
}
