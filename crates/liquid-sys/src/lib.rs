//! Raw FFI bindings to vendored liquid-dsp.
//!
//! Hand-written (no bindgen). Symbols are added incrementally as the safe
//! wrapper crate `liquid` needs them. F.1.0 scope: version + error query.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int};

extern "C" {
    pub static liquid_version: [c_char; 0];
    pub fn liquid_libversion() -> *const c_char;
    pub fn liquid_libversion_number() -> c_int;
    pub fn liquid_error_str(code: c_int) -> *const c_char;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn version_string_is_non_empty() {
        let s = unsafe { CStr::from_ptr(liquid_libversion()) };
        assert!(!s.to_bytes().is_empty());
    }
}
