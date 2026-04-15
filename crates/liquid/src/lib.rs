//! Safe wrapper around liquid-dsp.
//!
//! F.1.1 scope: linear modem (BPSK/QPSK/PSK8) + multi-stage arbitrary
//! resampler (CRCF). Follow-up wrappers (symsync, agc, nco, firpfbch2,
//! AX.25) land as the digital pipeline in `arion-core` grows.

use std::ffi::CStr;

pub mod error;
pub mod modem;
pub mod msresamp;
pub mod nco;

pub use error::LiquidError;
pub use modem::{Modem, ModemScheme};
pub use msresamp::{Complex32, MsResamp};
pub use nco::Nco;

pub fn version() -> &'static str {
    unsafe {
        CStr::from_ptr(liquid_sys::liquid_libversion())
            .to_str()
            .unwrap_or("unknown")
    }
}
