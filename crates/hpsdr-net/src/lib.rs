//! UDP transport for HPSDR Protocol 1 radios (Metis / Hermes / HermesLite 2).
//!
//! This crate layers three pieces on top of `hpsdr-protocol`:
//!
//! - [`discover`] — broadcast UDP discovery, returns every radio seen
//!   within a timeout window.
//! - [`Session`] — starts a running RX session against a single radio:
//!   spawns a receive thread that decodes Metis packets into an `rtrb`
//!   SPSC ring of [`hpsdr_protocol::IqSample`], and a control thread that
//!   lets the caller push frequency / sample-rate updates to the radio.
//! - [`MockHl2`] — an in-process fake HermesLite 2 that answers discovery
//!   and streams synthetic data frames, used by this crate's integration
//!   tests and by `thetis-core`'s smoke tests. Handy for CI since it
//!   doesn't need any real hardware.
//!
//! Phase A scope: RX only, one receiver, 48 kHz, no TX audio path yet.

#![forbid(unsafe_code)]

use std::io;

pub mod discovery;
pub mod mock;
pub mod session;

pub use discovery::{discover, DiscoveryOptions, RadioInfo};
pub use mock::MockHl2;
pub use session::{Session, SessionConfig, SessionCommand, SessionStatus};

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("protocol error: {0}")]
    Protocol(#[from] hpsdr_protocol::ProtocolError),

    #[error("session already stopped")]
    AlreadyStopped,

    #[error("radio did not acknowledge start after {attempts} attempts")]
    StartFailed { attempts: u32 },
}

/// Default UDP port Protocol 1 radios listen on (both for discovery and
/// for the Start / Stop / data stream).
pub const HPSDR_PORT: u16 = 1024;
