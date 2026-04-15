//! Raw FFI bindings to WSJT-X's `wsprd` decoder — **skeleton**.
//!
//! The C source is vendored under `vendor/wsprd/` (copy of
//! `lib/wsprd/` from the WSJT-X sourceforge repository, GPLv3),
//! but no Rust code uses it yet. The crate exists so the Rust
//! workspace has a stable name to depend on once the library
//! refactor (see `build.rs`) is done.
//!
//! Status tracked in `todo/other_modes.md` (phase G.1 decoder
//! step). The pipeline-side stub that will call into this crate
//! lives in `arion-core::digital::wspr::decode_slot`.

#![allow(non_camel_case_types)]
#![allow(dead_code)]
