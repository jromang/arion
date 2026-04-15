//! Raw FFI bindings to WSJT-X's wsprd helpers.
//!
//! We only link the FFTW-free subset (Fano decoder, metric tables,
//! callsign hash, unpacker, encoder utilities). The full wsprd.c
//! file scanner isn't wrapped — Arion's Rust side handles the
//! spectral search and 4-FSK demod itself, then hands soft bits to
//! `fano()` and the decoded bytes to `unpk_()`.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use std::os::raw::{c_char, c_int, c_uint};

// --- Fano soft-decision decoder (fano.c) ------------------------------------

extern "C" {
    /// Fano sequential decoder for K=32 rate-1/2 convolutional code.
    /// Returns 0 on success, non-zero when the decoder gives up.
    /// - `data`: output bytes (nbits info bits packed big-endian).
    /// - `symbols`: input 2×nbits soft bytes, 0..255.
    /// - `mettab`: 2×256 metric table.
    pub fn fano(
        metric: *mut c_uint,
        cycles: *mut c_uint,
        maxnp: *mut c_uint,
        data: *mut u8,
        symbols: *mut u8,
        nbits: c_uint,
        mettab: *mut [c_int; 256], // int[2][256] → pointer to row of 256
        delta: c_int,
        maxcycles: c_uint,
    ) -> c_int;

    /// 2×256 metric table pre-computed for typical SNRs. `int[2][256]`.
    pub static mut mettab: [[c_int; 256]; 2];
}

// --- Deinterleave + unpack (wsprd_utils.c) ---------------------------------

extern "C" {
    /// Undo the WSPR bit-reversal interleaver in place over 162 bytes.
    pub fn deinterleave(sym: *mut u8);

    /// Unpack the 50-bit raw WSPR message into `call_loc_pow`
    /// ("CALLSIGN LOCATOR POWER" / "TYPE1" variants). Returns 0 on
    /// a well-formed message, non-zero on rejection.
    pub fn unpk_(
        message: *mut i8,
        hashtab: *mut c_char,
        loctab: *mut c_char,
        call_loc_pow: *mut c_char,
        callsign: *mut c_char,
    ) -> c_int;
}

// --- Encoder (wsprsim_utils.c) --------------------------------------------

extern "C" {
    /// Pack a "CALL LOCATOR POWER" text message into 162 channel
    /// tones (0..3 for 4-FSK). Returns 0 on success, non-zero on
    /// packing failure. `hashtab` / `loctab` are a 32768×{13,5}
    /// scratch area used by the callsign hash table.
    pub fn get_wspr_channel_symbols(
        rawmessage: *const c_char,
        hashtab: *mut c_char,
        loctab: *mut c_char,
        symbols: *mut u8,
    ) -> c_int;
}
