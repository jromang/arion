//! Raw FFI bindings to the vendored WDSP C library.
//!
//! Phase A: hand-written declarations for the handful of WDSP entry points
//! `arion-core` actually calls (channel lifecycle, mode/rates, `fexchange0`,
//! analyzer, a couple of metering functions). When the surface grows in
//! phase B we'll switch to `bindgen` against `comm.h`, but the hand-rolled
//! form is clearer for now and avoids pulling bindgen into the build.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]

use std::os::raw::{c_char, c_double, c_float, c_int};

// --- Constants mirrored from comm.h -----------------------------------

/// Maximum number of simultaneously open channels (`MAX_CHANNELS` in comm.h).
pub const MAX_CHANNELS: c_int = 32;

/// WDSP channel type: `0` for RXA (receive), `1` for TXA (transmit).
pub const CHANNEL_TYPE_RX: c_int = 0;
pub const CHANNEL_TYPE_TX: c_int = 1;

// --- RX demodulator modes (RXA.h, enum RXA_MODE) ----------------------
//
// Upstream doesn't #define these symbolically — they're magic integers
// passed to SetRXAMode. Values taken from RXA.c's switch statements.
pub const RXA_MODE_LSB:  c_int = 0;
pub const RXA_MODE_USB:  c_int = 1;
pub const RXA_MODE_DSB:  c_int = 2;
pub const RXA_MODE_CWL:  c_int = 3;
pub const RXA_MODE_CWU:  c_int = 4;
pub const RXA_MODE_FM:   c_int = 5;
pub const RXA_MODE_AM:   c_int = 6;
pub const RXA_MODE_DIGU: c_int = 7;
pub const RXA_MODE_SPEC: c_int = 8;
pub const RXA_MODE_DIGL: c_int = 9;
pub const RXA_MODE_SAM:  c_int = 10;
pub const RXA_MODE_DRM:  c_int = 11;

// --- WDSP functions ---------------------------------------------------

unsafe extern "C" {
    /// Allocate a channel and spin up its DSP thread.
    ///
    /// Upstream signature (channel.h):
    /// ```c
    /// void OpenChannel(int channel, int in_size, int dsp_size,
    ///                  int input_samplerate, int dsp_rate, int output_samplerate,
    ///                  int type, int state,
    ///                  double tdelayup, double tslewup,
    ///                  double tdelaydown, double tslewdown,
    ///                  int bfo);
    /// ```
    pub fn OpenChannel(
        channel: c_int,
        in_size: c_int,
        dsp_size: c_int,
        input_samplerate: c_int,
        dsp_rate: c_int,
        output_samplerate: c_int,
        ch_type: c_int,
        state: c_int,
        tdelayup: c_double,
        tslewup: c_double,
        tdelaydown: c_double,
        tslewdown: c_double,
        bfo: c_int,
    );

    /// Tear down a channel allocated by `OpenChannel`. Blocks until the DSP
    /// thread exits.
    pub fn CloseChannel(channel: c_int);

    /// Enable/disable a channel. Returns the previous state.
    pub fn SetChannelState(channel: c_int, state: c_int, dmode: c_int) -> c_int;

    /// Change the RX demodulation mode (one of `RXA_MODE_*`).
    pub fn SetRXAMode(channel: c_int, mode: c_int);

    /// Change the input sample rate (Hz).
    pub fn SetInputSamplerate(channel: c_int, samplerate: c_int);

    /// Change the DSP-side sample rate (Hz).
    pub fn SetDSPSamplerate(channel: c_int, samplerate: c_int);

    /// Change the output sample rate (Hz).
    pub fn SetOutputSamplerate(channel: c_int, samplerate: c_int);

    /// Push an input IQ buffer into a channel and pull the matching output
    /// buffer out.
    ///
    /// Both buffers are **interleaved complex doubles** — pairs of
    /// `(I, Q)`. Sizes come from the channel's `in_size` / `out_size`
    /// passed to `OpenChannel`: the input buffer is `in_size` complex
    /// samples (`2 * in_size` doubles), the output is `out_size` complex
    /// samples. Upstream upgrades to double precision inside RXA so this
    /// interface is the only way in and out — there is no `float*`
    /// alternative.
    pub fn fexchange0(channel: c_int, in_buf: *mut c_double, out_buf: *mut c_double, error: *mut c_int);

    // --- RXA tuning / filtering -----------------------------------------

    /// Set the bandpass filter edges (Hz, relative to the baseband
    /// carrier) for the currently selected RXA mode. For USB you might
    /// pass `(200.0, 3000.0)`; for LSB negate both.
    pub fn RXASetPassband(channel: c_int, f_low: c_double, f_high: c_double);

    /// Direct setter for the narrow-band passband filter used by most
    /// demodulators. Same parameter meaning as [`RXASetPassband`].
    pub fn RXANBPSetFreqs(channel: c_int, f_low: c_double, f_high: c_double);

    // --- RXA AGC --------------------------------------------------------

    /// AGC mode: `0` = OFF, `1` = LONG, `2` = SLOW, `3` = MEDIUM, `4` = FAST.
    pub fn SetRXAAGCMode(channel: c_int, mode: c_int);

    /// Maximum AGC gain in dB (typically `120.0`).
    pub fn SetRXAAGCTop(channel: c_int, max_agc: c_double);

    // --- RXA output gain ------------------------------------------------

    /// Panel gain — linear scale factor applied to the RX audio after
    /// demodulation and before handing the samples back through
    /// `fexchange0`. `1.0` is unity.
    pub fn SetRXAPanelGain1(channel: c_int, gain: c_double);

    /// Enable/disable true stereo (binaural) output. `0` = mono copied
    /// to both channels, `1` = independent L/R (used only by CW binaural
    /// or multi-RX modes).
    pub fn SetRXAPanelBinaural(channel: c_int, binaural: c_int);

    // --- TX Compressor (CFCOMP) -----------------------------------------

    /// Enable/disable the TX frequency-domain compander.
    pub fn SetTXACFCOMPRun(channel: c_int, run: c_int);

    /// Set the pre-compression gain in dB.
    pub fn SetTXACFCOMPPrecomp(channel: c_int, precomp: c_double);

    /// Enable/disable the TX graphic equalizer.
    pub fn SetTXAEQRun(channel: c_int, run: c_int);

    /// Set a 10-band TX graphic EQ (same format as RX).
    pub fn SetTXAGrphEQ10(channel: c_int, txeq: *const c_int);

    /// Set TX panel gain (mic gain, linear).
    pub fn SetTXAPanelGain1(channel: c_int, gain: c_double);

    // --- EQ (Graphic Equalizer) -----------------------------------------

    /// Enable/disable the RX graphic equalizer.
    pub fn SetRXAEQRun(channel: c_int, run: c_int);

    /// Set a 10-band graphic EQ. `rxeq` is a pointer to 11 ints:
    /// `rxeq[0]` = preamp gain (dB), `rxeq[1..=10]` = band gains
    /// at 32, 63, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz.
    pub fn SetRXAGrphEQ10(channel: c_int, rxeq: *const c_int);

    // --- ANF (Auto Notch Filter) ----------------------------------------

    /// Turn the LMS auto-notch filter on (`1`) or off (`0`).
    pub fn SetRXAANFRun(channel: c_int, run: c_int);

    // --- SNBA (Spectral Noise Blanker) ----------------------------------

    /// Turn the spectral noise blanker on (`1`) or off (`0`).
    pub fn SetRXASNBARun(channel: c_int, run: c_int);

    // --- EMNR (Enhanced Spectral Noise Reduction) -----------------------

    /// Toggle EMNR (Thetis-branded "NR2"). Runs the `emnr` module
    /// that was wired into the RXA pipeline at channel creation.
    pub fn SetRXAEMNRRun(channel: c_int, run: c_int);

    // --- ANR (Adaptive Noise Reduction) ---------------------------------

    /// Toggle LMS adaptive noise reduction (Thetis-branded "NR").
    pub fn SetRXAANRRun(channel: c_int, run: c_int);

    // --- NR3 (RNNoise) --------------------------------------------------
    //
    // These are only meaningful when `wdsp-sys` was built against a
    // real `rnnoise` system library (see `build.rs` pkg-config probe).
    // When the lib is missing, the stub implementations in
    // `shim/wdsp_nr_stubs.c` make the setters silent no-ops — they
    // still link so downstream Rust code can call them unconditionally.

    /// Turn the RNNoise denoiser on (`1`) or off (`0`) for the given
    /// RX channel.
    pub fn SetRXARNNRRun(channel: c_int, run: c_int);

    /// Where the denoiser sits in the RXA signal chain. Accepted
    /// values mirror upstream's `SetRXARNNRPosition`:
    /// `0` = post-filter, `1` = pre-filter. Most users want the
    /// default (0).
    pub fn SetRXARNNRPosition(channel: c_int, position: c_int);

    /// Whether to use RNNoise's built-in auto-gain output stage.
    /// `0` = raw VAD-weighted output, `1` = auto-gain. The factory
    /// default in upstream Arion is `1`.
    pub fn SetRXARNNRUseDefaultGain(channel: c_int, use_default_gain: c_int);

    // --- NR4 (libspecbleach) -------------------------------------------
    //
    // Like the NR3 calls above, these are silent no-ops when `wdsp-sys`
    // is built without `libspecbleach`.

    pub fn SetRXASBNRRun(channel: c_int, run: c_int);
    pub fn SetRXASBNRPosition(channel: c_int, position: c_int);
    pub fn SetRXASBNRreductionAmount(channel: c_int, amount: c_float);
    pub fn SetRXASBNRsmoothingFactor(channel: c_int, factor: c_float);
    pub fn SetRXASBNRwhiteningFactor(channel: c_int, factor: c_float);
    pub fn SetRXASBNRnoiseRescale(channel: c_int, factor: c_float);
    pub fn SetRXASBNRpostFilterThreshold(channel: c_int, threshold: c_float);
    pub fn SetRXASBNRnoiseScalingType(channel: c_int, scaling_type: c_int);
}

// --- Analyzer (spectrum display) API, subset from analyzer.h ----------

unsafe extern "C" {
    /// Configure an analyzer instance. The real signature is long — see
    /// `analyzer.h:SetAnalyzer` — phase A just wraps the raw form.
    #[allow(clippy::too_many_arguments)]
    pub fn SetAnalyzer(
        disp: c_int,
        n_pixout: c_int,
        n_fft: c_int,
        typ: c_int,
        flp: *mut c_int,
        sz: c_int,
        bfsz: c_int,
        window_type: c_int,
        kaiser_pi: c_double,
        overlap: c_int,
        clip: c_int,
        span_clip_l: c_int,
        span_clip_h: c_int,
        pixels: c_int,
        stitches: c_int,
        avm: c_int,
        av_mode: *mut c_int,
        av_backmult: *mut c_double,
        n_pix_avg: c_int,
        n_lin_avg: c_int,
        spur_elim_tau: *mut c_double,
        fscLin: c_double,
        fscHin: c_double,
        max_w: c_int,
    );

    /// Copy the latest pixel frame out of an analyzer instance.
    pub fn GetPixels(disp: c_int, pixout: c_int, pix: *mut c_float, flag: *mut c_int);
}

// --- Wisdom / FFTW plan cache -----------------------------------------
//
// Upstream `wisdom.c` pre-computes every FFT plan WDSP might ever need
// (sizes 64..MAX_WISDOM_SIZE = 262144, forward and backward, complex and
// real) and serialises them to a single file via FFTW's wisdom mechanism.
//
// The heavy lifting is hidden behind one exported function —
// `WDSPwisdom(directory)` — that either imports an existing
// `wdspWisdom00` file from `directory` or, if absent, rebuilds the full
// table (slow: 30s+ on first run) and exports it. Subsequent calls find
// the file and return immediately.
//
// **Important**: `directory` must end with the platform path separator;
// upstream does a plain `strcat` with the filename. The Rust wrapper in
// [`wdsp`](../../wdsp) takes care of that.

unsafe extern "C" {
    /// Prime (or rebuild) the FFTW wisdom cache.
    ///
    /// Returns `0` if wisdom was imported from the existing file and `1`
    /// if the cache was rebuilt from scratch. A `1` return on the first
    /// run is normal; subsequent runs should always return `0`.
    pub fn WDSPwisdom(directory: *const c_char) -> c_int;

    /// Human-readable status line set by the last (or in-progress) call
    /// to `WDSPwisdom`. Useful for a "building FFT plans…" progress
    /// label in the UI. The returned pointer is owned by WDSP's global
    /// `static char status[128]` — do not free.
    pub fn wisdom_get_status() -> *mut c_char;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: open an RX channel, flip state, close it. Exercises the
    /// FFI + vendored library + portability shim end-to-end without needing
    /// real IQ input. If this panics on startup it usually means the shim
    /// lost a symbol or the C library failed to initialise.
    /// Exercises OpenChannel → parameter setup → fexchange0 loop →
    /// CloseChannel against the vendored WDSP library plus the
    /// portability shim. This is *one* test function rather than two
    /// because FFTW plan creation is not thread-safe and cargo's default
    /// runner will happily execute `#[test]` fns in parallel, which
    /// corrupts WDSP's global plan cache and crashes with a glibc
    /// "corrupted size vs. prev_size" abort. Phase B wraps FFTW plan
    /// creation in a mutex inside the `wdsp` safe wrapper so multiple
    /// channels can be opened from different threads cleanly.
    ///
    /// If this ever regresses: the most common cause is a missing
    /// symbol / wrong signature in the shim, which typically shows up
    /// as either a link error or a heap-corruption abort inside the
    /// wdspmain DSP thread.
    #[test]
    fn rx_channel_lifecycle_and_process() {
        const IN_SIZE: usize = 1024;
        let mut in_buf  = vec![0.0_f64; 2 * IN_SIZE]; // interleaved I/Q
        let mut out_buf = vec![0.0_f64; 2 * IN_SIZE];
        let mut error   = 0_i32;

        unsafe {
            OpenChannel(
                /* channel          */ 0,
                /* in_size          */ IN_SIZE as c_int,
                /* dsp_size         */ IN_SIZE as c_int,
                /* input_samplerate */ 48_000,
                /* dsp_rate         */ 48_000,
                /* output_samplerate*/ 48_000,
                /* type             */ CHANNEL_TYPE_RX,
                /* state            */ 1,
                /* tdelayup         */ 0.0,
                /* tslewup          */ 0.0,
                /* tdelaydown       */ 0.0,
                /* tslewdown        */ 0.0,
                /* bfo              */ 0,
            );
            SetRXAMode(0, RXA_MODE_USB);
            RXASetPassband(0, 200.0, 3000.0);
            SetRXAAGCMode(0, 3);
            SetRXAPanelGain1(0, 1.0);

            // Push a few buffers of silence through the channel. Upstream
            // `bfo = 0` means early calls return `error = -2` ("no output
            // samples ready yet") — both 0 and -2 are benign.
            for _ in 0..4 {
                fexchange0(0, in_buf.as_mut_ptr(), out_buf.as_mut_ptr(), &mut error);
                assert!(
                    error == 0 || error == -2,
                    "unexpected fexchange0 error: {error}"
                );
            }

            CloseChannel(0);
        }
    }
}
