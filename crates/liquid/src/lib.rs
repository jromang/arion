//! Safe wrapper around liquid-dsp.
//!
//! F.1.0 scope: skeleton. Wrappers (modem, symsync, agc, nco, msresamp,
//! firpfbch, ax25) arrive in F.1.1 alongside their required FFI symbols.

use std::ffi::CStr;

pub mod error;

pub use error::LiquidError;

pub fn version() -> &'static str {
    unsafe {
        CStr::from_ptr(liquid_sys::liquid_libversion())
            .to_str()
            .unwrap_or("unknown")
    }
}
