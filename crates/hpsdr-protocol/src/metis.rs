//! 1032-byte Metis data packets and their two 512-byte USB frames.

use crate::control::CommandFrame;
use crate::sample::{
    sample_wire_stride, samples_per_usb_frame_n, IqSample, MultiIqSample,
    SAMPLES_PER_USB_FRAME, SAMPLE_WIRE_LEN,
};
use crate::ProtocolError;

/// Full size of a Metis data packet on the wire.
pub const METIS_PACKET_LEN: usize = 1032;

/// Size of each of the two USB frames embedded in a Metis data packet.
pub const USB_FRAME_LEN: usize = 512;

/// Byte offset of the first USB frame inside a Metis packet.
pub const USB_FRAME_0_OFFSET: usize = 8;

/// Byte offset of the second USB frame inside a Metis packet.
pub const USB_FRAME_1_OFFSET: usize = USB_FRAME_0_OFFSET + USB_FRAME_LEN;

/// Number of bytes in a USB frame's samples section (`= 504`).
pub const SAMPLES_SECTION_LEN: usize = SAMPLES_PER_USB_FRAME * SAMPLE_WIRE_LEN;

/// USB-framed endpoints inside a Metis packet (byte `[3]`).
///
/// Endpoint numbers originate from the legacy OpenHPSDR Metis board
/// where the FPGA spoke to the PC via a USB bridge with several logical
/// endpoints. The Ethernet transport carries them over unchanged:
///
/// - **endpoint 2** (`HostCommandAndTx`) — *host → radio*. Carries TX I/Q
///   samples plus the C0..C4 command word used to write radio registers
///   (frequency, sample rate, antenna routing…). This is what we use for
///   every outbound data packet, including the C&C frames sent during the
///   Start handshake.
/// - **endpoint 4** (`Wideband`) — wideband raw spectrum samples.
/// - **endpoint 6** (`RadioRxAndStatus`) — *radio → host*. Primary **RX
///   data** path: each packet carries 2 × 63 IQ samples plus status bytes
///   in the C0..C4 field (PTT, ADC overload, PLL lock, etc.). Upstream's
///   `networkproto1.c` sometimes calls this "status" because the C0 field
///   changed meaning vs the outbound form, but the rest of the frame is
///   the RX sample stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// Endpoint 2 — host-to-radio TX data + command-and-control writes.
    HostCommandAndTx,
    /// Endpoint 4 — wideband spectrum samples.
    Wideband,
    /// Endpoint 6 — radio-to-host RX samples + status.
    RadioRxAndStatus,
    /// Anything else the radio happens to emit. Preserved rather than
    /// rejected so a firmware upgrade doesn't turn into a crash.
    Other(u8),
}

impl Endpoint {
    pub fn from_wire(b: u8) -> Self {
        match b {
            2 => Endpoint::HostCommandAndTx,
            4 => Endpoint::Wideband,
            6 => Endpoint::RadioRxAndStatus,
            other => Endpoint::Other(other),
        }
    }

    pub fn to_wire(self) -> u8 {
        match self {
            Endpoint::HostCommandAndTx => 2,
            Endpoint::Wideband         => 4,
            Endpoint::RadioRxAndStatus => 6,
            Endpoint::Other(b)         => b,
        }
    }

    /// True for any endpoint whose payload is a pair of 512-byte USB
    /// frames carrying 63 IQ samples each — i.e. everything except a
    /// non-data `Other`. Used by the session handshake to decide
    /// whether a packet counts as "the radio started streaming".
    pub fn carries_iq_samples(self) -> bool {
        matches!(
            self,
            Endpoint::HostCommandAndTx
                | Endpoint::RadioRxAndStatus
                | Endpoint::Wideband
        )
    }
}

/// A parsed USB frame: its command word plus the raw 504-byte sample block.
///
/// Samples are kept as raw bytes inside the frame so that a client that
/// only needs I/Q can skip decoding the mic audio (and vice-versa). Use the
/// iterator methods below to walk them as [`IqSample`] on demand.
#[derive(Debug, Clone)]
pub struct UsbFrame {
    pub command: CommandFrame,
    pub samples: [u8; SAMPLES_SECTION_LEN],
}

impl Default for UsbFrame {
    fn default() -> Self {
        UsbFrame {
            command: CommandFrame::default(),
            samples: [0u8; SAMPLES_SECTION_LEN],
        }
    }
}

impl UsbFrame {
    /// Decode one USB frame from a 512-byte slice. Verifies the `7F 7F 7F`
    /// sync at the start.
    pub fn parse(buf: &[u8; USB_FRAME_LEN], offset: usize) -> Result<Self, ProtocolError> {
        if buf[0] != 0x7F || buf[1] != 0x7F || buf[2] != 0x7F {
            return Err(ProtocolError::BadUsbFrameSync {
                offset,
                found: [buf[0], buf[1], buf[2]],
            });
        }
        let command = CommandFrame {
            c0: crate::control::ControlByte(buf[3]),
            c1: buf[4],
            c2: buf[5],
            c3: buf[6],
            c4: buf[7],
        };
        let mut samples = [0u8; SAMPLES_SECTION_LEN];
        samples.copy_from_slice(&buf[8..USB_FRAME_LEN]);
        Ok(UsbFrame { command, samples })
    }

    /// Encode this frame into a caller-supplied 512-byte buffer.
    pub fn encode_into(&self, out: &mut [u8; USB_FRAME_LEN]) {
        out[0] = 0x7F;
        out[1] = 0x7F;
        out[2] = 0x7F;
        let mut cmd = [0u8; 5];
        self.command.encode_into(&mut cmd);
        out[3..8].copy_from_slice(&cmd);
        out[8..].copy_from_slice(&self.samples);
    }

    /// Walk the 63 IQ samples carried by this frame assuming `num_rx == 1`.
    /// Fast path with a fixed stride — prefer this when you know the
    /// session is single-RX. For multi-RX sessions use
    /// [`Self::iter_iq_multi`].
    pub fn iq_samples(&self) -> impl Iterator<Item = IqSample> + '_ {
        (0..SAMPLES_PER_USB_FRAME).map(move |n| {
            let start = n * SAMPLE_WIRE_LEN;
            let mut bytes = [0u8; SAMPLE_WIRE_LEN];
            bytes.copy_from_slice(&self.samples[start..start + SAMPLE_WIRE_LEN]);
            IqSample::from_bytes(&bytes)
        })
    }

    /// Walk the samples carried by this frame, decoding them as
    /// multi-RX samples. `num_rx` must match what the radio was
    /// configured with in register 0 — if it disagrees, the decoded
    /// values will be nonsense.
    ///
    /// Yields exactly [`samples_per_usb_frame_n(num_rx)`] items. Any
    /// trailing unused bytes in the frame payload are ignored.
    pub fn iter_iq_multi(
        &self,
        num_rx: usize,
    ) -> impl Iterator<Item = MultiIqSample> + '_ {
        let stride = sample_wire_stride(num_rx);
        let count  = samples_per_usb_frame_n(num_rx);
        (0..count).map(move |n| {
            let start = n * stride;
            MultiIqSample::from_bytes(&self.samples[start..start + stride], num_rx)
        })
    }

    /// Overwrite the 63 samples of this frame from an iterator of
    /// single-RX samples. The iterator must yield exactly
    /// `SAMPLES_PER_USB_FRAME` items; if it yields fewer, the remaining
    /// bytes of the frame are zeroed.
    pub fn fill_samples<I: IntoIterator<Item = IqSample>>(&mut self, it: I) {
        self.samples.fill(0);
        for (n, s) in it.into_iter().take(SAMPLES_PER_USB_FRAME).enumerate() {
            let start = n * SAMPLE_WIRE_LEN;
            self.samples[start..start + SAMPLE_WIRE_LEN].copy_from_slice(&s.to_bytes());
        }
    }

    /// Overwrite the samples of this frame with multi-RX data at the
    /// given receiver count. The iterator may yield fewer than
    /// `samples_per_usb_frame_n(num_rx)` items; in that case the rest
    /// of the frame is left zero.
    ///
    /// Every yielded sample's `num_rx` must match the `num_rx` argument
    /// — mismatches are a programmer error.
    pub fn fill_samples_multi<I: IntoIterator<Item = MultiIqSample>>(
        &mut self,
        num_rx: usize,
        it: I,
    ) {
        let stride = sample_wire_stride(num_rx);
        let count  = samples_per_usb_frame_n(num_rx);
        self.samples.fill(0);
        for (n, s) in it.into_iter().take(count).enumerate() {
            debug_assert_eq!(
                s.num_rx as usize, num_rx,
                "MultiIqSample.num_rx {} doesn't match caller num_rx {}",
                s.num_rx, num_rx,
            );
            let start = n * stride;
            s.to_bytes(&mut self.samples[start..start + stride]);
        }
    }
}

/// A full Metis data packet — eight-byte header and two USB frames.
#[derive(Debug, Clone)]
pub struct MetisPacket {
    pub endpoint: Endpoint,
    pub sequence: u32,
    pub frame0: UsbFrame,
    pub frame1: UsbFrame,
}

impl MetisPacket {
    /// Parse a 1032-byte UDP payload. Returns an error if the length or
    /// the sync bytes don't match, or if either USB frame's own sync is
    /// wrong.
    pub fn parse(data: &[u8]) -> Result<Self, ProtocolError> {
        if data.len() < METIS_PACKET_LEN {
            return Err(ProtocolError::Truncated {
                expected: METIS_PACKET_LEN,
                got: data.len(),
            });
        }
        if data[0] != 0xEF || data[1] != 0xFE {
            return Err(ProtocolError::WrongMagic {
                found: [data[0], data[1]],
            });
        }
        if data[2] != 0x01 {
            return Err(ProtocolError::WrongPacketType(data[2]));
        }

        let endpoint = Endpoint::from_wire(data[3]);
        let sequence = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);

        let mut frame0_buf = [0u8; USB_FRAME_LEN];
        frame0_buf.copy_from_slice(&data[USB_FRAME_0_OFFSET..USB_FRAME_0_OFFSET + USB_FRAME_LEN]);
        let frame0 = UsbFrame::parse(&frame0_buf, USB_FRAME_0_OFFSET)?;

        let mut frame1_buf = [0u8; USB_FRAME_LEN];
        frame1_buf.copy_from_slice(&data[USB_FRAME_1_OFFSET..USB_FRAME_1_OFFSET + USB_FRAME_LEN]);
        let frame1 = UsbFrame::parse(&frame1_buf, USB_FRAME_1_OFFSET)?;

        Ok(MetisPacket { endpoint, sequence, frame0, frame1 })
    }

    /// Serialise this packet into a fresh 1032-byte buffer.
    pub fn encode(&self) -> [u8; METIS_PACKET_LEN] {
        let mut out = [0u8; METIS_PACKET_LEN];
        out[0] = 0xEF;
        out[1] = 0xFE;
        out[2] = 0x01;
        out[3] = self.endpoint.to_wire();
        out[4..8].copy_from_slice(&self.sequence.to_be_bytes());

        let (_, rest) = out.split_at_mut(USB_FRAME_0_OFFSET);
        let (f0_slice, rest) = rest.split_at_mut(USB_FRAME_LEN);
        let (f1_slice, _)    = rest.split_at_mut(USB_FRAME_LEN);

        let f0: &mut [u8; USB_FRAME_LEN] = f0_slice.try_into().unwrap();
        let f1: &mut [u8; USB_FRAME_LEN] = f1_slice.try_into().unwrap();
        self.frame0.encode_into(f0);
        self.frame1.encode_into(f1);

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{register, ControlByte};

    fn empty_frame() -> UsbFrame {
        UsbFrame::default()
    }

    #[test]
    fn packet_roundtrip_preserves_header_and_frames() {
        let pkt = MetisPacket {
            endpoint: Endpoint::HostCommandAndTx,
            sequence: 0xDEAD_BEEF,
            frame0: UsbFrame {
                command: CommandFrame {
                    c0: ControlByte::new(register::CONFIG, false),
                    c1: 0x03, c2: 0x00, c3: 0x00, c4: 0x08,
                },
                samples: {
                    let mut s = [0u8; SAMPLES_SECTION_LEN];
                    for (i, b) in s.iter_mut().enumerate() { *b = (i & 0xFF) as u8; }
                    s
                },
            },
            frame1: empty_frame(),
        };

        let bytes = pkt.encode();
        assert_eq!(bytes.len(), METIS_PACKET_LEN);
        assert_eq!(&bytes[..8], &[0xEF, 0xFE, 0x01, 0x02, 0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(bytes[8], 0x7F);
        assert_eq!(bytes[9], 0x7F);
        assert_eq!(bytes[10], 0x7F);

        let back = MetisPacket::parse(&bytes).unwrap();
        assert_eq!(back.endpoint, Endpoint::HostCommandAndTx);
        assert_eq!(back.sequence, 0xDEAD_BEEF);
        assert_eq!(back.frame0.command.c0.register(), register::CONFIG);
        assert_eq!(back.frame0.samples, pkt.frame0.samples);
        assert_eq!(back.frame1.samples, pkt.frame1.samples);
    }

    #[test]
    fn parse_rejects_wrong_magic() {
        let mut buf = [0u8; METIS_PACKET_LEN];
        buf[0] = 0x12; buf[1] = 0x34; buf[2] = 0x01;
        assert!(matches!(
            MetisPacket::parse(&buf),
            Err(ProtocolError::WrongMagic { .. })
        ));
    }

    #[test]
    fn parse_rejects_wrong_packet_type() {
        let mut buf = [0u8; METIS_PACKET_LEN];
        buf[0] = 0xEF; buf[1] = 0xFE; buf[2] = 0x02;
        assert!(matches!(
            MetisPacket::parse(&buf),
            Err(ProtocolError::WrongPacketType(0x02))
        ));
    }

    #[test]
    fn parse_rejects_bad_usb_sync() {
        let pkt = MetisPacket {
            endpoint: Endpoint::HostCommandAndTx,
            sequence: 1,
            frame0: empty_frame(),
            frame1: empty_frame(),
        };
        let mut bytes = pkt.encode();
        bytes[USB_FRAME_1_OFFSET] = 0x00; // break the second frame's sync
        assert!(matches!(
            MetisPacket::parse(&bytes),
            Err(ProtocolError::BadUsbFrameSync { .. })
        ));
    }

    #[test]
    fn parse_rejects_truncated_packet() {
        let buf = [0u8; 1031];
        assert!(matches!(
            MetisPacket::parse(&buf),
            Err(ProtocolError::Truncated { expected: 1032, got: 1031 })
        ));
    }

    #[test]
    fn multi_rx_iter_roundtrip_num_rx_2() {
        // Build a frame with 36 samples at num_rx=2 carrying a
        // distinguishable pattern, encode it, decode via
        // iter_iq_multi, and check that every sample comes back
        // intact.
        use crate::sample::{samples_per_usb_frame_n, MAX_RX};
        let num_rx = 2;
        let count  = samples_per_usb_frame_n(num_rx);
        assert_eq!(count, 36);

        let make_sample = |n: usize| {
            let mut s = MultiIqSample {
                num_rx: num_rx as u8,
                mic:    n as i16,
                ..MultiIqSample::default()
            };
            s.rx[0] = ((n as f32) / 1000.0, -(n as f32) / 1000.0);
            s.rx[1] = ((n as f32) / 500.0,   (n as f32) / 2000.0);
            s
        };

        let mut frame = UsbFrame::default();
        frame.fill_samples_multi(num_rx, (0..count).map(make_sample));

        let decoded: Vec<_> = frame.iter_iq_multi(num_rx).collect();
        assert_eq!(decoded.len(), count);
        for (n, got) in decoded.iter().enumerate() {
            let expected = make_sample(n);
            assert_eq!(got.num_rx, expected.num_rx);
            assert_eq!(got.mic,    expected.mic);
            for r in 0..num_rx {
                assert!((got.rx[r].0 - expected.rx[r].0).abs() < 1e-4,
                    "sample {n} rx{r}.i: got {} expected {}",
                    got.rx[r].0, expected.rx[r].0);
                assert!((got.rx[r].1 - expected.rx[r].1).abs() < 1e-4,
                    "sample {n} rx{r}.q: got {} expected {}",
                    got.rx[r].1, expected.rx[r].1);
            }
            // Slots past num_rx are zeroed.
            for r in num_rx..MAX_RX {
                assert_eq!(got.rx[r], (0.0, 0.0));
            }
        }
    }

    #[test]
    fn iq_samples_iterate_all_63_per_frame() {
        let pkt = MetisPacket {
            endpoint: Endpoint::HostCommandAndTx,
            sequence: 0,
            frame0: {
                let mut f = empty_frame();
                f.fill_samples((0..SAMPLES_PER_USB_FRAME).map(|n| IqSample {
                    i: (n as f32) / 1000.0,
                    q: -(n as f32) / 1000.0,
                    mic: n as i16,
                }));
                f
            },
            frame1: empty_frame(),
        };
        let bytes = pkt.encode();
        let back = MetisPacket::parse(&bytes).unwrap();
        let samples: Vec<_> = back.frame0.iq_samples().collect();
        assert_eq!(samples.len(), SAMPLES_PER_USB_FRAME);
        for (n, s) in samples.iter().enumerate() {
            assert!((s.i - (n as f32) / 1000.0).abs() < 1e-4);
            assert!((s.q + (n as f32) / 1000.0).abs() < 1e-4);
            assert_eq!(s.mic, n as i16);
        }
    }
}
