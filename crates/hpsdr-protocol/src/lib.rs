//! HPSDR Protocol 1 packet types and codec.
//!
//! This crate is pure data: no I/O, no threads. It defines the wire format
//! used by Metis / Hermes / HermesLite 2 / Angelia / Orion over Ethernet and
//! provides encoders and decoders for the pieces `hpsdr-net` needs.
//!
//! # Wire format cheatsheet
//!
//! **Discovery request** (63 bytes, UDP broadcast to port 1024):
//! ```text
//! [0]     0xEF
//! [1]     0xFE
//! [2]     0x02    (discovery)
//! [3..63] zeros
//! ```
//!
//! **Discovery reply** (≥24 bytes, UDP back to requester):
//! ```text
//! [0]     0xEF
//! [1]     0xFE
//! [2]     0x02 idle / 0x03 busy
//! [3..9]  MAC address (6 bytes)
//! [9]     firmware / code version
//! [10]    board type (HPSDRHW enum, HL2 = 6)
//! [14..20] mercury + penny + metis versions
//! [20]    num_rxs (phase-2 firmwares only)
//! ```
//!
//! **Start / stop** (64 bytes, unicast to port 1024):
//! ```text
//! [0]     0xEF
//! [1]     0xFE
//! [2]     0x04
//! [3]     0x01 start / 0x00 stop
//! [4..64] zeros
//! ```
//!
//! **Data frame** (1032 bytes, UDP, both directions once running):
//! ```text
//! [0..2]   0xEF 0xFE
//! [2]      0x01   (data packet)
//! [3]      endpoint (2 = I/Q+audio, 4 = wideband, 6 = status)
//! [4..8]   sequence number, big-endian u32
//! [8..520] USB frame 0
//! [520..1032] USB frame 1
//! ```
//!
//! Each **USB frame** (512 bytes) carries 63 IQ samples plus a 5-byte control
//! word:
//! ```text
//! [0..3]    0x7F 0x7F 0x7F   (sync)
//! [3]       C0               ((register << 1) | MOX)
//! [4..8]    C1..C4           (register payload)
//! [8..512]  63 × 8 bytes of { I24 BE, Q24 BE, mic16 BE }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

pub mod control;
pub mod discovery;
pub mod metis;
pub mod sample;

pub use control::{register, CommandFrame, ControlByte, StartCommand, StopCommand};
pub use discovery::{DiscoveryRequest, DiscoveryReply, HpsdrModel};
pub use metis::{Endpoint, MetisPacket, UsbFrame, METIS_PACKET_LEN, USB_FRAME_LEN};
pub use sample::{IqSample, SAMPLES_PER_USB_FRAME};

/// Errors produced by decoders in this crate. Encoders never fail.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A byte slice handed to a decoder was shorter than required.
    #[error("packet too short: expected at least {expected} bytes, got {got}")]
    Truncated { expected: usize, got: usize },

    /// The leading two bytes did not match the expected `0xEF 0xFE` sync.
    #[error("wrong magic: expected 0xEF 0xFE, got {found:02X?}")]
    WrongMagic { found: [u8; 2] },

    /// A data packet did not declare itself as type `0x01`.
    #[error("expected data packet type 0x01, got 0x{0:02X}")]
    WrongPacketType(u8),

    /// A USB frame in a data packet was missing its `0x7F 0x7F 0x7F` sync.
    #[error("bad USB frame sync at offset {offset}: {found:02X?}")]
    BadUsbFrameSync { offset: usize, found: [u8; 3] },

    /// The discovery reply's type byte was neither idle (`0x02`) nor busy (`0x03`).
    #[error("unknown discovery reply status byte 0x{0:02X}")]
    UnknownDiscoveryStatus(u8),
}
