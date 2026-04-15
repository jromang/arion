//! Safe wrappers over WSJT-X's wsprd helpers.
//!
//! The Arion side (in `arion-core::digital::wspr`) does frequency
//! search + 4-FSK demod in pure Rust, then hands 162 soft-bit
//! bytes to [`fano_decode`] to run the Layland-Lushbaugh K=32
//! rate-1/2 sequential decoder, and the resulting 81-bit payload
//! to [`unpack`] for the callsign / locator / power split.
//!
//! A matching encoder path ([`channel_symbols`]) is exposed so
//! round-trip tests can build a known reference signal without
//! shelling out to `wsprsim`.

use std::ffi::{CStr, CString};

use thiserror::Error;
use wsprd_sys as sys;

#[derive(Debug, Error)]
pub enum WsprError {
    #[error("message contains a NUL byte")]
    MessageContainsNul,
    #[error("get_wspr_channel_symbols failed (rc={0})")]
    Encode(i32),
    #[error("fano decoder gave up")]
    FanoFailed,
    #[error("unpk_ rejected the decoded bytes")]
    UnpackFailed,
}

/// WSPR transmits 162 channel symbols (4-FSK, values 0..=3).
pub const N_SYMBOLS: usize = 162;
/// Rate-1/2 convolutional code: 81 info bits → 162 coded bits.
pub const N_BITS: usize = 81;

// Scratch tables used by both the encoder and unpacker for
// callsign hashing. 32768 × 13 bytes + 32768 × 5 bytes total.
struct Scratch {
    hashtab: Vec<u8>,
    loctab: Vec<u8>,
}

impl Scratch {
    fn new() -> Self {
        Self {
            hashtab: vec![0u8; 32_768 * 13],
            loctab: vec![0u8; 32_768 * 5],
        }
    }
}

/// Pack a `"CALLSIGN LOCATOR POWER"` text message into the 162
/// channel tones (0..=3) that WSPR transmits.
pub fn channel_symbols(message: &str) -> Result<[u8; N_SYMBOLS], WsprError> {
    let c = CString::new(message).map_err(|_| WsprError::MessageContainsNul)?;
    let mut sc = Scratch::new();
    let mut out = [0u8; N_SYMBOLS];
    let rc = unsafe {
        sys::get_wspr_channel_symbols(
            c.as_ptr(),
            sc.hashtab.as_mut_ptr() as *mut _,
            sc.loctab.as_mut_ptr() as *mut _,
            out.as_mut_ptr(),
        )
    };
    // get_wspr_channel_symbols returns 0 on malformed message,
    // non-zero on success (the final `return 1` at the end of
    // wsprsim_utils.c:get_wspr_channel_symbols).
    if rc == 0 {
        return Err(WsprError::Encode(rc));
    }
    Ok(out)
}

/// Undo the WSPR bit-reversal interleaver in place over 162 soft
/// bytes.
pub fn deinterleave(symbols: &mut [u8; N_SYMBOLS]) {
    unsafe { sys::deinterleave(symbols.as_mut_ptr()) };
}

/// Run the Fano sequential decoder on 162 soft bytes (0..=255) and
/// return the 81-bit payload packed into 11 bytes (big-endian, last
/// 7 bits of byte 10 are padding).
pub fn fano_decode(soft_bits: &mut [u8; N_SYMBOLS]) -> Result<[u8; 11], WsprError> {
    let mut metric = 0u32;
    let mut cycles = 0u32;
    let mut maxnp = 0u32;
    let mut decdata = [0u8; 11];
    let rc = unsafe {
        sys::fano(
            &mut metric,
            &mut cycles,
            &mut maxnp,
            decdata.as_mut_ptr(),
            soft_bits.as_mut_ptr(),
            N_BITS as u32,
            std::ptr::addr_of_mut!(sys::mettab) as *mut [i32; 256],
            60,
            10_000,
        )
    };
    if rc != 0 {
        return Err(WsprError::FanoFailed);
    }
    Ok(decdata)
}

/// Unpack the 81-bit Fano output into a human-readable
/// `"CALLSIGN LOCATOR POWER"` string (the WSPR spot format).
pub fn unpack(decdata: &[u8; 11]) -> Result<String, WsprError> {
    let mut sc = Scratch::new();
    let mut call_loc_pow = [0i8; 23];
    let mut callsign = [0i8; 13];
    // unpk_ wants a *mutable* message pointer for historical reasons
    // but only reads from it. Clone into a local buffer.
    let mut message = [0i8; 11];
    for (i, b) in decdata.iter().enumerate() {
        message[i] = *b as i8;
    }
    let rc = unsafe {
        sys::unpk_(
            message.as_mut_ptr(),
            sc.hashtab.as_mut_ptr() as *mut _,
            sc.loctab.as_mut_ptr() as *mut _,
            call_loc_pow.as_mut_ptr(),
            callsign.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return Err(WsprError::UnpackFailed);
    }
    let s = unsafe { CStr::from_ptr(call_loc_pow.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end C round-trip: encode → (no channel) → fano path.
    ///
    /// The encoder builds 162 4-FSK symbols (values 0..=3). Each
    /// symbol carries one sync bit (top) and one data bit (bottom).
    /// Extracting the data bit as a hard soft-byte (0 → 0, 1 → 255),
    /// deinterleaving, and running the Fano decoder must recover
    /// the original message through `unpk_`.
    #[test]
    fn encode_fano_roundtrip() {
        let msg = "AA0AA EM15 37";
        let channels = channel_symbols(msg).unwrap();

        let mut soft = [0u8; N_SYMBOLS];
        // Channel symbol t = 2 * data_bit + sync_bit (per
        // wsprsim_utils.c line 307), so recover the data bit as
        // the high bit of the symbol.
        for (i, &c) in channels.iter().enumerate() {
            soft[i] = if c >> 1 == 1 { 255 } else { 0 };
        }
        deinterleave(&mut soft);
        let decdata = fano_decode(&mut soft).unwrap();
        let text = unpack(&decdata).unwrap();
        assert!(text.contains("AA0AA"), "text={text:?}");
        assert!(text.contains("EM15"), "text={text:?}");
    }
}
