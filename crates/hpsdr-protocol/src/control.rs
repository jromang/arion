//! Protocol 1 control byte (`C0`) encoding, named register addresses, and
//! the Start / Stop control datagrams.
//!
//! # Control register layout
//!
//! Every outgoing USB frame carries one "command word":
//!
//! ```text
//! C0 = (register_address << 1) | (mox ? 1 : 0)
//! C1, C2, C3, C4 = 4-byte register payload, big-endian
//! ```
//!
//! Incoming frames use the same `C0` byte but the `mox` bit carries radio
//! status (e.g. PTT pressed via hand mic).

/// Length of the Start and Stop command packets.
pub const COMMAND_PACKET_LEN: usize = 64;

/// Symbolic register addresses as defined by the HPSDR Protocol 1 spec.
///
/// Only the subset needed to configure a basic RX session is listed here;
/// the rest will be added as phase B brings TX and multi-RX online.
pub mod register {
    /// Sample rate / 10 MHz ref / config in `C1..C4` of the first frame.
    pub const CONFIG: u8 = 0x00;
    /// Transmit NCO frequency (Hz, 32-bit big-endian).
    pub const TX_NCO: u8 = 0x01;
    /// Receiver 1 NCO frequency.
    pub const RX1_NCO: u8 = 0x02;
    /// Receiver 2 NCO frequency.
    pub const RX2_NCO: u8 = 0x03;
    /// Receiver 3 NCO frequency.
    pub const RX3_NCO: u8 = 0x04;
    /// Receiver 4 NCO frequency.
    pub const RX4_NCO: u8 = 0x05;
    /// Receiver 5 NCO frequency.
    pub const RX5_NCO: u8 = 0x06;
    /// Receiver 6 NCO frequency.
    pub const RX6_NCO: u8 = 0x07;
    /// Receiver 7 NCO frequency.
    pub const RX7_NCO: u8 = 0x08;
    /// Alex / antenna routing / preamps.
    pub const ALEX: u8 = 0x09;
}

/// A C0 byte: 7-bit register address plus the 1-bit MOX flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlByte(pub u8);

impl ControlByte {
    /// Build a C0 from a register address and the MOX bit.
    pub const fn new(register: u8, mox: bool) -> Self {
        ControlByte((register << 1) | (mox as u8))
    }

    /// Decode the register address carried in bits 7..1 of this byte.
    pub const fn register(self) -> u8 {
        self.0 >> 1
    }

    /// `true` if the radio (inbound) or host (outbound) is asserting MOX /
    /// "transmit now".
    pub const fn mox(self) -> bool {
        (self.0 & 0x01) != 0
    }
}

/// A command word (C0..C4) ready to be written into a USB frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandFrame {
    pub c0: ControlByte,
    pub c1: u8,
    pub c2: u8,
    pub c3: u8,
    pub c4: u8,
}

impl CommandFrame {
    /// Pack a 32-bit frequency into a `register` write.
    ///
    /// C1..C4 carry the frequency as a big-endian u32, MSB first.
    pub fn set_frequency(register: u8, freq_hz: u32) -> Self {
        let [c1, c2, c3, c4] = freq_hz.to_be_bytes();
        CommandFrame {
            c0: ControlByte::new(register, false),
            c1,
            c2,
            c3,
            c4,
        }
    }

    /// Pack a `register` write with a 4-byte raw payload.
    pub fn raw(register: u8, mox: bool, payload: [u8; 4]) -> Self {
        CommandFrame {
            c0: ControlByte::new(register, mox),
            c1: payload[0],
            c2: payload[1],
            c3: payload[2],
            c4: payload[3],
        }
    }

    /// Write this command frame into a 5-byte slice (`C0..C4`).
    pub fn encode_into(self, out: &mut [u8; 5]) {
        out[0] = self.c0.0;
        out[1] = self.c1;
        out[2] = self.c2;
        out[3] = self.c3;
        out[4] = self.c4;
    }
}

/// The "Start data" command sent to `radio_ip:1024`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StartCommand;

impl StartCommand {
    pub fn encode(self) -> [u8; COMMAND_PACKET_LEN] {
        let mut buf = [0u8; COMMAND_PACKET_LEN];
        buf[0] = 0xEF;
        buf[1] = 0xFE;
        buf[2] = 0x04;
        buf[3] = 0x01;
        buf
    }
}

/// The "Stop data" command sent to `radio_ip:1024`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StopCommand;

impl StopCommand {
    pub fn encode(self) -> [u8; COMMAND_PACKET_LEN] {
        let mut buf = [0u8; COMMAND_PACKET_LEN];
        buf[0] = 0xEF;
        buf[1] = 0xFE;
        buf[2] = 0x04;
        buf[3] = 0x00;
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_byte_roundtrip() {
        for reg in 0u8..=0x3F {
            for mox in [false, true] {
                let cb = ControlByte::new(reg, mox);
                assert_eq!(cb.register(), reg);
                assert_eq!(cb.mox(), mox);
            }
        }
    }

    #[test]
    fn set_frequency_packs_big_endian() {
        // FT8 calling frequency on 40m, chosen because it's the kind of
        // round-number value that's easy to mis-hand-convert. The wire
        // form is the IEEE 32-bit big-endian representation of 7_074_000.
        let cmd = CommandFrame::set_frequency(register::RX1_NCO, 7_074_000);
        assert_eq!(cmd.c0.register(), register::RX1_NCO);
        assert!(!cmd.c0.mox());
        let got = [cmd.c1, cmd.c2, cmd.c3, cmd.c4];
        assert_eq!(got, 7_074_000_u32.to_be_bytes());
    }

    #[test]
    fn start_command_matches_upstream() {
        let buf = StartCommand.encode();
        assert_eq!(buf.len(), 64);
        assert_eq!(&buf[..4], &[0xEF, 0xFE, 0x04, 0x01]);
        assert!(buf[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn stop_command_matches_upstream() {
        let buf = StopCommand.encode();
        assert_eq!(buf.len(), 64);
        assert_eq!(&buf[..4], &[0xEF, 0xFE, 0x04, 0x00]);
        assert!(buf[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn encode_into_writes_five_bytes() {
        let cmd = CommandFrame::raw(register::CONFIG, false, [0x03, 0x00, 0x00, 0x08]);
        let mut out = [0u8; 5];
        cmd.encode_into(&mut out);
        assert_eq!(out, [0x00, 0x03, 0x00, 0x00, 0x08]);
    }
}
