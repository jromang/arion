//! wsprd-sys build script — placeholder.
//!
//! Actually compiling WSJT-X's wsprd from `vendor/wsprd/` needs two
//! pieces of work not done yet:
//!
//! 1. A library entry point. `wsprd.c` is a CLI with `main()` that
//!    reads a WAV / .c2 file. We need to extract the core decode
//!    loop into e.g. `wsprd_decode_samples(const float *samples,
//!    int nsamples, float dial_hz, WsprDecode *out, int max_out)`
//!    so the shim can call it with a buffer instead of a file.
//! 2. FFTW linking. wsprd uses `fftwf_*` (single-precision). Arion
//!    already vendors FFTW via `wdsp-sys`, so we want to reuse
//!    that rather than ship a second copy. Either expose
//!    `DEP_WDSP_SYS_FFTW_DIR` from `wdsp-sys` and read it here, or
//!    ship a tiny `fftwf_* → kiss_fft` shim (kiss_fft is already
//!    vendored under `ft8-sys/vendor/ft8_lib/fft/`).
//!
//! Meanwhile this crate is a *source vendor only*: the sources are
//! checked in so a future session can touch them with no download
//! step, and the skeleton gives the Rust side a stable name
//! (`wsprd-sys`) to depend on once the C work is done.

fn main() {
    println!("cargo:rerun-if-changed=vendor/wsprd");
    println!("cargo:rerun-if-changed=build.rs");

    if cfg!(feature = "build-c") {
        panic!(
            "wsprd-sys 'build-c' feature is not implemented yet; \
             see build.rs for the remaining work."
        );
    }
}
