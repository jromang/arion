//! Safe wrapper around ft8_lib.
//!
//! Two pieces are useful to Arion:
//!
//! - [`encode_to_audio`] builds a full 12 kHz FT8 signal for a text
//!   message. Used by the self-generated round-trip test and to
//!   sanity-check the receiver.
//! - [`Monitor`] is a stateful decoder: feed it 12 kHz audio blocks,
//!   call [`Monitor::decode`] at the end of a 15-second slot to
//!   collect the messages it heard.

use ft8_sys as sys;
use std::ffi::{CStr, CString};

pub mod error;

pub use error::Ft8Error;

const FT8_SYMBOLS: usize = 79;
const FT8_SYMBOL_PERIOD: f32 = 0.16; // seconds
/// Canonical base frequency of the FT8 sub-band.
pub const DEFAULT_BASE_FREQ_HZ: f32 = 1_000.0;

/// Encode `message_text` ("CQ F4XYZ JN06" etc.) as a 12 kHz FT8
/// audio stream. The signal is placed at `base_freq_hz` (tone 0).
/// GFSK shaping per the FT8 spec: Gaussian-filtered frequency
/// transitions with a BT = 2 raised-cosine pulse.
pub fn encode_to_audio(message_text: &str, base_freq_hz: f32) -> Result<Vec<f32>, Ft8Error> {
    // Pack text → 77-bit payload.
    let mut msg = sys::ftx_message_t::default();
    let c = CString::new(message_text).map_err(|_| Ft8Error::InvalidText)?;
    let rc = unsafe {
        sys::ftx_message_encode(&mut msg, std::ptr::null_mut(), c.as_ptr())
    };
    if rc != 0 {
        return Err(Ft8Error::EncodeFailed(rc));
    }
    // Payload → 79 tones (0..7).
    let mut tones = [0u8; FT8_SYMBOLS];
    unsafe { sys::ft8_encode(msg.payload.as_ptr(), tones.as_mut_ptr()) };

    // GFSK synthesis delegated to the shim wrapper that mirrors
    // demo/gen_ft8.c:synth_gfsk byte-for-byte. This avoids subtle
    // divergences between a hand-ported Rust version and the C
    // reference that powers the decoder.
    let sample_rate = 12_000_i32;
    let symbol_period = FT8_SYMBOL_PERIOD;
    let symbol_bt = 2.0_f32;
    let nspsym = (0.5 + sample_rate as f32 * symbol_period) as usize;
    let n_wave = FT8_SYMBOLS * nspsym;
    let mut pulse_scratch = vec![0.0_f32; 3 * nspsym];
    let mut dphi_scratch = vec![0.0_f32; n_wave + 2 * nspsym];
    let mut audio = vec![0.0_f32; n_wave];
    unsafe {
        sys::arion_ft8_synth_gfsk(
            tones.as_ptr(),
            FT8_SYMBOLS as i32,
            base_freq_hz,
            symbol_bt,
            symbol_period,
            sample_rate,
            pulse_scratch.as_mut_ptr(),
            dphi_scratch.as_mut_ptr(),
            audio.as_mut_ptr(),
        );
    }
    Ok(audio)
}

/// FT8 receiver. `storage` is allocated with 8-byte alignment
/// because monitor_t contains double-aligned fields (function
/// pointers, kiss_fft config).
pub struct Monitor {
    storage: Box<[u64]>,
    block_size: usize,
}

impl Monitor {
    /// Build a 12 kHz FT8 monitor watching 300–3000 Hz.
    pub fn new() -> Result<Self, Ft8Error> {
        let size = unsafe { sys::arion_ft8_monitor_sizeof() };
        let words = size.div_ceil(8);
        let mut storage: Box<[u64]> = vec![0u64; words].into_boxed_slice();
        let cfg = sys::monitor_config_t {
            f_min: 200.0,
            f_max: 3_000.0,
            sample_rate: 12_000,
            time_osr: 2,
            freq_osr: 2,
            protocol: sys::FTX_PROTOCOL_FT8,
        };
        unsafe { sys::monitor_init(storage.as_mut_ptr() as *mut _, &cfg) };
        // monitor_process consumes `time_osr × subblock_size` = one
        // full symbol period per call (each call runs time_osr inner
        // FFTs). The subblock_size is block_size / time_osr.
        let block_size = (cfg.sample_rate as f32 * FT8_SYMBOL_PERIOD) as usize;
        Ok(Self {
            storage,
            block_size,
        })
    }

    /// Number of samples expected by each `process()` call.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Feed exactly `block_size()` samples at 12 kHz.
    pub fn process(&mut self, block: &[f32]) {
        assert_eq!(block.len(), self.block_size);
        unsafe {
            sys::monitor_process(self.storage.as_mut_ptr() as *mut _, block.as_ptr());
        }
    }

    pub fn reset(&mut self) {
        unsafe { sys::monitor_reset(self.storage.as_mut_ptr() as *mut _) };
    }

    /// Try to decode messages from the waterfall accumulated so far.
    /// Typically called at the end of a 15-second slot.
    pub fn decode(&mut self, max_candidates: usize, min_score: i32) -> Vec<Decode> {
        let wf_ptr = unsafe {
            sys::arion_ft8_monitor_waterfall(self.storage.as_mut_ptr() as *const _)
        };
        let mut heap = vec![sys::ftx_candidate_t::default(); max_candidates];
        let n_found = unsafe {
            sys::ftx_find_candidates(wf_ptr, max_candidates as i32, heap.as_mut_ptr(), min_score)
        };
        let mut out = Vec::new();
        // Sort by score descending so the first decode wins.
        let mut sorted: Vec<_> = heap[..n_found as usize].to_vec();
        sorted.sort_by_key(|c| -(c.score as i32));
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for cand in &sorted {
            let mut msg = sys::ftx_message_t::default();
            let mut status = sys::ftx_decode_status_t::default();
            let ok = unsafe { sys::ftx_decode_candidate(wf_ptr, cand, 25, &mut msg, &mut status) };
            if !ok {
                continue;
            }
            let mut buf = [0i8; 64];
            let mut offsets = sys::ftx_message_offsets_t::default();
            let rc = unsafe {
                sys::ftx_message_decode(&msg, std::ptr::null_mut(), buf.as_mut_ptr(), &mut offsets)
            };
            if rc != 0 {
                continue;
            }
            let text = unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            // Multiple candidates often point at the same message;
            // dedupe by text so the UI shows each QSO once.
            if !seen.insert(text.clone()) {
                continue;
            }
            out.push(Decode {
                text,
                // ft8_lib's ftx_decode_status_t carries no SNR field.
                // Use the candidate's sync score as a reasonable
                // proxy (roughly monotonic with SNR for identical
                // waterfalls).
                score: cand.score as i32,
                freq_hz: status.freq,
                time_offset_s: status.time,
            });
        }
        out
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe { sys::monitor_free(self.storage.as_mut_ptr() as *mut _) };
    }
}

#[derive(Debug, Clone)]
pub struct Decode {
    pub text: String,
    /// Sync score reported by ft8_lib's find_candidates. Not a true
    /// SNR in dB, but monotonic with signal quality — useful as a
    /// display rank.
    pub score: i32,
    pub freq_hz: f32,
    pub time_offset_s: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_produces_expected_length() {
        let audio = encode_to_audio("CQ AA0AA FN42", DEFAULT_BASE_FREQ_HZ).unwrap();
        // 79 symbols × 1920 samples @ 12 kHz ≈ 12.6 s of signal.
        assert_eq!(audio.len(), 79 * 1920);
    }

    #[test]
    fn round_trip_decode() {
        let mut m = Monitor::new().unwrap();
        let audio = encode_to_audio("CQ AA0AA FN42", DEFAULT_BASE_FREQ_HZ).unwrap();
        // 79 symbols × time_osr = 158 subblocks of signal, plus a
        // short guard of leading + trailing silence (mirrors a real
        // slot where TX starts ~0.5 s after the UTC boundary and
        // leaves trailing margin before the next slot).
        let guard = vec![0.0_f32; m.block_size() * 3];
        for chunk in guard.chunks_exact(m.block_size()) {
            m.process(chunk);
        }
        for chunk in audio.chunks_exact(m.block_size()) {
            m.process(chunk);
        }
        for chunk in guard.chunks_exact(m.block_size()) {
            m.process(chunk);
        }
        let decodes = m.decode(64, 10);
        let joined: String = decodes.iter().map(|d| d.text.as_str()).collect();
        assert!(
            joined.contains("AA0AA") || joined.contains("FN42"),
            "no expected text in: {joined:?}"
        );
    }

    #[test]
    fn monitor_accepts_expected_block_size() {
        let mut m = Monitor::new().unwrap();
        // One full FT8 symbol @ 12 kHz = 1920 samples.
        assert_eq!(m.block_size(), 1920);
        let block = vec![0.0_f32; m.block_size()];
        m.process(&block);
    }

    #[test]
    fn decode_silence_returns_empty() {
        let mut m = Monitor::new().unwrap();
        let block = vec![0.0_f32; m.block_size()];
        for _ in 0..20 {
            m.process(&block);
        }
        let decodes = m.decode(16, 10);
        assert!(decodes.is_empty());
    }
}
