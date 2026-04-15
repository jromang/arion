use crate::error::LiquidError;
use liquid_sys as sys;

pub use sys::LiquidFloatComplex as Complex32;

/// Multi-stage arbitrary-rate resampler over complex float samples.
pub struct MsResamp {
    raw: sys::msresamp_crcf,
    rate: f32,
}

impl MsResamp {
    /// `rate` = output/input (e.g. `0.25` for 48 k → 12 k).
    /// `stop_band_db` = stop-band attenuation, typically 60.
    pub fn new(rate: f32, stop_band_db: f32) -> Result<Self, LiquidError> {
        if rate <= 0.0 {
            return Err(LiquidError::InvalidArgument("rate must be > 0"));
        }
        let raw = unsafe { sys::msresamp_crcf_create(rate, stop_band_db) };
        if raw.is_null() {
            return Err(LiquidError::InvalidArgument(
                "msresamp_crcf_create returned null",
            ));
        }
        Ok(Self { raw, rate })
    }

    pub fn rate(&self) -> f32 {
        self.rate
    }

    pub fn num_output(&self, num_input: u32) -> u32 {
        unsafe { sys::msresamp_crcf_get_num_output(self.raw, num_input) }
    }

    /// Execute on `input`, writing into `output`. Returns the number of
    /// output samples produced. Allocate output with capacity
    /// >= `num_output(input.len())` (or `ceil(1 + 2 * rate * nx)`).
    pub fn execute(&mut self, input: &[Complex32], output: &mut [Complex32]) -> usize {
        let mut ny: u32 = 0;
        unsafe {
            sys::msresamp_crcf_execute(
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
        unsafe { sys::msresamp_crcf_reset(self.raw) };
    }
}

impl Drop for MsResamp {
    fn drop(&mut self) {
        unsafe { sys::msresamp_crcf_destroy(self.raw) };
    }
}

unsafe impl Send for MsResamp {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimate_4x() {
        let mut r = MsResamp::new(0.25, 60.0).unwrap();
        let input = vec![Complex32 { re: 1.0, im: 0.0 }; 4096];
        let cap = r.num_output(input.len() as u32) as usize;
        let mut out = vec![Complex32 { re: 0.0, im: 0.0 }; cap + 16];
        let n = r.execute(&input, &mut out);
        let expected = input.len() / 4;
        assert!(n + 16 >= expected && n <= expected + 16, "n={n}");
    }
}
