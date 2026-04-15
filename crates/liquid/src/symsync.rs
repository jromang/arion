use crate::error::LiquidError;
use crate::msresamp::Complex32;
use liquid_sys as sys;

/// Symbol synchronizer (complex float) using Kaiser polyphase filter
/// banks. Consumes `k` samples per symbol and produces 1 output sample
/// per symbol once locked.
pub struct SymSync {
    raw: sys::symsync_crcf,
}

impl SymSync {
    /// Create a synchronizer.
    ///
    /// - `k`: input samples per symbol (≥ 2; typically 8)
    /// - `m`: symbol delay of the internal filter (typical 3)
    /// - `beta`: filter rolloff, 0..=1 (typical 0.3)
    /// - `num_filters`: size of the polyphase bank (typical 32)
    pub fn new_kaiser(k: u32, m: u32, beta: f32, num_filters: u32) -> Result<Self, LiquidError> {
        let raw = unsafe { sys::symsync_crcf_create_kaiser(k, m, beta, num_filters) };
        if raw.is_null() {
            return Err(LiquidError::InvalidArgument(
                "symsync_crcf_create_kaiser returned null",
            ));
        }
        // Output 1 sample per symbol.
        unsafe { sys::symsync_crcf_set_output_rate(raw, 1) };
        Ok(Self { raw })
    }

    /// Loop-filter bandwidth, 0..=1. Smaller = slower tracking but
    /// more noise-immune.
    pub fn set_loop_bandwidth(&mut self, bt: f32) {
        unsafe { sys::symsync_crcf_set_lf_bw(self.raw, bt) };
    }

    /// Run on `input` at input rate; writes at most one output per
    /// `k` input samples. Returns the number of output symbols.
    pub fn execute(&mut self, input: &[Complex32], output: &mut [Complex32]) -> usize {
        let mut ny: u32 = 0;
        unsafe {
            sys::symsync_crcf_execute(
                self.raw,
                input.as_ptr() as *mut Complex32,
                input.len() as u32,
                output.as_mut_ptr(),
                &mut ny,
            );
        }
        ny as usize
    }

    pub fn reset(&mut self) {
        unsafe { sys::symsync_crcf_reset(self.raw) };
    }
}

impl Drop for SymSync {
    fn drop(&mut self) {
        unsafe { sys::symsync_crcf_destroy(self.raw) };
    }
}

unsafe impl Send for SymSync {}
