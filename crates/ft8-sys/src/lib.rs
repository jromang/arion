//! Raw FFI bindings to vendored ft8_lib (Kārlis Goba / KGoba).
//!
//! Hand-written (no bindgen). Symbols are added incrementally as the
//! safe wrapper crate `ft8` needs them.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_float, c_int, c_void};

// --- Waterfall / candidates / decode status ------------------------------

pub const FTX_PROTOCOL_FT4: c_int = 0;
pub const FTX_PROTOCOL_FT8: c_int = 1;
pub type ftx_protocol_t = c_int;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct ftx_waterfall_t {
    pub max_blocks: c_int,
    pub num_blocks: c_int,
    pub num_bins: c_int,
    pub time_osr: c_int,
    pub freq_osr: c_int,
    pub mag: *mut u8,
    pub block_stride: c_int,
    pub protocol: ftx_protocol_t,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct ftx_candidate_t {
    pub score: i16,
    pub time_offset: i16,
    pub freq_offset: i16,
    pub time_sub: u8,
    pub freq_sub: u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct ftx_decode_status_t {
    pub freq: c_float,
    pub time: c_float,
    pub ldpc_errors: c_int,
    pub crc_extracted: u16,
    pub crc_calculated: u16,
}

// --- monitor (audio → waterfall) ----------------------------------------

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct monitor_config_t {
    pub f_min: c_float,
    pub f_max: c_float,
    pub sample_rate: c_int,
    pub time_osr: c_int,
    pub freq_osr: c_int,
    pub protocol: ftx_protocol_t,
}

// monitor_t layout is private; we keep it opaque via Box<u8>.
pub type monitor_t = c_void;

extern "C" {
    pub fn arion_ft8_monitor_sizeof() -> usize;
    pub fn arion_ft8_monitor_waterfall(me: *const monitor_t) -> *const ftx_waterfall_t;
    pub fn monitor_init(me: *mut monitor_t, cfg: *const monitor_config_t);
    pub fn monitor_reset(me: *mut monitor_t);
    pub fn monitor_process(me: *mut monitor_t, frame: *const c_float);
    pub fn monitor_free(me: *mut monitor_t);

    pub fn ftx_find_candidates(
        power: *const ftx_waterfall_t,
        num_candidates: c_int,
        heap: *mut ftx_candidate_t,
        min_score: c_int,
    ) -> c_int;
}

// --- message struct (opaque to sys, size-matched to ft8_lib) -------------

/// ft8_lib's `ftx_message_t` is `uint8_t payload[10] + uint16_t hash`.
/// We over-allocate 4 bytes for alignment / forward-compatibility.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ftx_message_t {
    pub payload: [u8; 10],
    pub hash: u16,
    _reserved: [u8; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct ftx_message_offsets_t {
    pub offsets: [i16; 6],
}

extern "C" {
    pub fn ft8_encode(payload: *const u8, tones: *mut u8);
    pub fn ftx_message_encode(
        msg: *mut ftx_message_t,
        hash_if: *mut c_void,
        message_text: *const c_char,
    ) -> c_int;
}

extern "C" {
    pub fn ftx_message_init(msg: *mut ftx_message_t);
    pub fn ftx_message_decode(
        msg: *const ftx_message_t,
        hash_if: *mut c_void,
        message: *mut c_char,
        offsets: *mut ftx_message_offsets_t,
    ) -> c_int;

    pub fn ftx_decode_candidate(
        power: *const ftx_waterfall_t,
        cand: *const ftx_candidate_t,
        max_iterations: c_int,
        message: *mut ftx_message_t,
        status: *mut ftx_decode_status_t,
    ) -> bool;
}
