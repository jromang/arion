use crate::error::LiquidError;
use liquid_sys as sys;

/// Linear modulation scheme.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ModemScheme {
    Bpsk,
    Qpsk,
    Psk8,
}

impl ModemScheme {
    fn to_sys(self) -> sys::modulation_scheme {
        match self {
            Self::Bpsk => sys::LIQUID_MODEM_BPSK,
            Self::Qpsk => sys::LIQUID_MODEM_QPSK,
            Self::Psk8 => sys::LIQUID_MODEM_PSK8,
        }
    }
}

/// Linear modem (modulator + demodulator) over complex float samples.
pub struct Modem {
    raw: sys::modemcf,
}

impl Modem {
    pub fn new(scheme: ModemScheme) -> Result<Self, LiquidError> {
        let raw = unsafe { sys::modemcf_create(scheme.to_sys()) };
        if raw.is_null() {
            return Err(LiquidError::InvalidArgument("modemcf_create returned null"));
        }
        Ok(Self { raw })
    }

    pub fn bits_per_symbol(&self) -> u32 {
        unsafe { sys::modemcf_get_bps(self.raw) }
    }

    pub fn modulate(&mut self, symbol: u32) -> (f32, f32) {
        let mut out = sys::LiquidFloatComplex::default();
        unsafe { sys::modemcf_modulate(self.raw, symbol, &mut out) };
        (out.re, out.im)
    }

    pub fn demodulate(&mut self, sample: (f32, f32)) -> u32 {
        let sym = sys::LiquidFloatComplex {
            re: sample.0,
            im: sample.1,
        };
        let mut out: u32 = 0;
        unsafe { sys::modemcf_demodulate(self.raw, sym, &mut out) };
        out
    }

    pub fn evm(&self) -> f32 {
        unsafe { sys::modemcf_get_demodulator_evm(self.raw) }
    }

    pub fn phase_error(&self) -> f32 {
        unsafe { sys::modemcf_get_demodulator_phase_error(self.raw) }
    }
}

impl Drop for Modem {
    fn drop(&mut self) {
        unsafe { sys::modemcf_destroy(self.raw) };
    }
}

// Safety: modemcf handles wrap a plain struct with no hidden thread state;
// each Modem owns its handle exclusively.
unsafe impl Send for Modem {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpsk_roundtrip() {
        let mut m = Modem::new(ModemScheme::Bpsk).unwrap();
        assert_eq!(m.bits_per_symbol(), 1);
        for sym in 0..2 {
            let s = m.modulate(sym);
            let got = m.demodulate(s);
            assert_eq!(got, sym);
        }
    }

    #[test]
    fn qpsk_roundtrip() {
        let mut m = Modem::new(ModemScheme::Qpsk).unwrap();
        assert_eq!(m.bits_per_symbol(), 2);
        for sym in 0..4 {
            let s = m.modulate(sym);
            let got = m.demodulate(s);
            assert_eq!(got, sym);
        }
    }
}
