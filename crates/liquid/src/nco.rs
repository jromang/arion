use crate::error::LiquidError;
use liquid_sys as sys;

use crate::msresamp::Complex32;

/// Numerically-controlled oscillator (complex) over float samples.
pub struct Nco {
    raw: sys::nco_crcf,
}

impl Nco {
    pub fn new() -> Result<Self, LiquidError> {
        let raw = unsafe { sys::nco_crcf_create(sys::LIQUID_NCO) };
        if raw.is_null() {
            return Err(LiquidError::InvalidArgument(
                "nco_crcf_create returned null",
            ));
        }
        Ok(Self { raw })
    }

    /// Set the NCO frequency in radians per sample.
    pub fn set_frequency_radians(&mut self, dtheta: f32) {
        unsafe { sys::nco_crcf_set_frequency(self.raw, dtheta) };
    }

    /// Convenience: set frequency from a target Hz and sample rate.
    pub fn set_frequency_hz(&mut self, hz: f32, sample_rate_hz: f32) {
        let dtheta = 2.0 * std::f32::consts::PI * hz / sample_rate_hz;
        self.set_frequency_radians(dtheta);
    }

    /// Rotate `x` down by the current phase, then advance the NCO.
    pub fn mix_down_step(&mut self, x: Complex32) -> Complex32 {
        let mut y = Complex32::default();
        unsafe {
            sys::nco_crcf_mix_down(self.raw, x, &mut y);
            sys::nco_crcf_step(self.raw);
        }
        y
    }

    pub fn reset(&mut self) {
        unsafe { sys::nco_crcf_reset(self.raw) };
    }
}

impl Drop for Nco {
    fn drop(&mut self) {
        unsafe { sys::nco_crcf_destroy(self.raw) };
    }
}

unsafe impl Send for Nco {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_down_dc_when_matched() {
        let mut n = Nco::new().unwrap();
        let fs = 12_000.0_f32;
        n.set_frequency_hz(1500.0, fs);
        // Generate a 1500 Hz tone and mix it down; result should trend
        // towards DC (near-zero phase rotation).
        let mut last = Complex32::default();
        for k in 0..128 {
            let t = k as f32 / fs;
            let phi = 2.0 * std::f32::consts::PI * 1500.0 * t;
            let x = Complex32 {
                re: phi.cos(),
                im: phi.sin(),
            };
            last = n.mix_down_step(x);
        }
        // After 128 samples the mixed-down signal should be near (1,0).
        assert!(last.re > 0.9, "re={}", last.re);
        assert!(last.im.abs() < 0.2, "im={}", last.im);
    }
}
