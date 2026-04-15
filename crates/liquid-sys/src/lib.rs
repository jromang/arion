//! Raw FFI bindings to vendored liquid-dsp.
//!
//! Hand-written (no bindgen). Symbols are added incrementally as the safe
//! wrapper crate `liquid` needs them.
//!
//! F.1.1 scope: linear modem (modemcf_*) + multi-stage resampler
//! (msresamp_crcf_*). Upcoming: symsync_crcf, agc_crcf, nco_crcf,
//! firpfbch2_crcf, AX.25.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_float, c_int, c_uint, c_void};

/// C99 `float _Complex` on liquid's supported platforms is two contiguous
/// floats laid out as (real, imag). Wrap as a `#[repr(C)]` struct so
/// clippy's `improper_ctypes` lint is satisfied.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct LiquidFloatComplex {
    pub re: c_float,
    pub im: c_float,
}

// --- library metadata -------------------------------------------------------

extern "C" {
    pub fn liquid_libversion() -> *const c_char;
    pub fn liquid_libversion_number() -> c_int;
    pub fn liquid_error_str(code: c_int) -> *const c_char;
}

// --- linear modem (modemcf) -------------------------------------------------

pub type modemcf = *mut c_void;

pub type modulation_scheme = c_int;
pub const LIQUID_MODEM_UNKNOWN: modulation_scheme = 0;
pub const LIQUID_MODEM_PSK2: modulation_scheme = 1;
pub const LIQUID_MODEM_PSK4: modulation_scheme = 2;
pub const LIQUID_MODEM_PSK8: modulation_scheme = 3;
// Specific schemes — see liquid.h modulation_scheme enum.
// Offset = 1(UNKNOWN) + 8(PSK) + 8(DPSK) + 8(ASK) + 7(QAM) + 7(APSK).
pub const LIQUID_MODEM_BPSK: modulation_scheme = 39;
pub const LIQUID_MODEM_QPSK: modulation_scheme = 40;

extern "C" {
    pub fn modemcf_create(scheme: modulation_scheme) -> modemcf;
    pub fn modemcf_destroy(q: modemcf) -> c_int;
    pub fn modemcf_reset(q: modemcf) -> c_int;
    pub fn modemcf_get_bps(q: modemcf) -> c_uint;
    pub fn modemcf_modulate(q: modemcf, symbol: c_uint, y: *mut LiquidFloatComplex) -> c_int;
    pub fn modemcf_demodulate(q: modemcf, x: LiquidFloatComplex, s: *mut c_uint) -> c_int;
    pub fn modemcf_get_demodulator_evm(q: modemcf) -> c_float;
    pub fn modemcf_get_demodulator_phase_error(q: modemcf) -> c_float;
    pub fn modemcf_get_demodulator_sample(q: modemcf, x_hat: *mut LiquidFloatComplex) -> c_int;
}

// --- symbol synchronizer (symsync_crcf) -------------------------------------

pub type symsync_crcf = *mut c_void;

extern "C" {
    pub fn symsync_crcf_create_kaiser(
        k: c_uint,
        m: c_uint,
        beta: c_float,
        num_filters: c_uint,
    ) -> symsync_crcf;
    pub fn symsync_crcf_destroy(q: symsync_crcf) -> c_int;
    pub fn symsync_crcf_reset(q: symsync_crcf) -> c_int;
    pub fn symsync_crcf_set_output_rate(q: symsync_crcf, k_out: c_uint) -> c_int;
    pub fn symsync_crcf_set_lf_bw(q: symsync_crcf, bt: c_float) -> c_int;
    pub fn symsync_crcf_execute(
        q: symsync_crcf,
        x: *mut LiquidFloatComplex,
        nx: c_uint,
        y: *mut LiquidFloatComplex,
        ny: *mut c_uint,
    ) -> c_int;
}

// --- NCO / PLL (nco_crcf) ---------------------------------------------------

pub type nco_crcf = *mut c_void;

pub type liquid_ncotype = c_int;
pub const LIQUID_NCO: liquid_ncotype = 0;

extern "C" {
    pub fn nco_crcf_create(ty: liquid_ncotype) -> nco_crcf;
    pub fn nco_crcf_destroy(q: nco_crcf) -> c_int;
    pub fn nco_crcf_reset(q: nco_crcf) -> c_int;
    pub fn nco_crcf_set_frequency(q: nco_crcf, dtheta: c_float) -> c_int;
    pub fn nco_crcf_step(q: nco_crcf) -> c_int;
    pub fn nco_crcf_mix_down(
        q: nco_crcf,
        x: LiquidFloatComplex,
        y: *mut LiquidFloatComplex,
    ) -> c_int;
}

// --- multi-stage arbitrary resampler (msresamp_crcf) ------------------------

pub type msresamp_crcf = *mut c_void;

extern "C" {
    pub fn msresamp_crcf_create(rate: c_float, stop_band_db: c_float) -> msresamp_crcf;
    pub fn msresamp_crcf_destroy(q: msresamp_crcf) -> c_int;
    pub fn msresamp_crcf_reset(q: msresamp_crcf) -> c_int;
    pub fn msresamp_crcf_get_rate(q: msresamp_crcf) -> c_float;
    pub fn msresamp_crcf_get_delay(q: msresamp_crcf) -> c_float;
    pub fn msresamp_crcf_get_num_output(q: msresamp_crcf, num_input: c_uint) -> c_uint;
    pub fn msresamp_crcf_execute(
        q: msresamp_crcf,
        x: *mut LiquidFloatComplex,
        nx: c_uint,
        y: *mut LiquidFloatComplex,
        ny: *mut c_uint,
    ) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn version_string_is_non_empty() {
        let s = unsafe { CStr::from_ptr(liquid_libversion()) };
        assert!(!s.to_bytes().is_empty());
    }

    #[test]
    fn bpsk_modem_roundtrip() {
        unsafe {
            let q = modemcf_create(LIQUID_MODEM_BPSK);
            assert!(!q.is_null());
            let mut sym = LiquidFloatComplex::default();
            modemcf_modulate(q, 1, &mut sym);
            let mut out: c_uint = 99;
            modemcf_demodulate(q, sym, &mut out);
            let _ = &sym;
            assert_eq!(out, 1);
            modemcf_destroy(q);
        }
    }

    #[test]
    fn msresamp_4_to_1_ratio() {
        unsafe {
            let q = msresamp_crcf_create(0.25, 60.0);
            assert!(!q.is_null());
            assert!((msresamp_crcf_get_rate(q) - 0.25).abs() < 1e-3);
            msresamp_crcf_destroy(q);
        }
    }
}
