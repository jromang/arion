//! Protocol 1 discovery: broadcast a 63-byte request on UDP port 1024,
//! parse the replies coming back from every radio on the subnet.

use crate::ProtocolError;

/// Length of the discovery request packet sent to the radio.
pub const DISCOVERY_REQUEST_LEN: usize = 63;

/// Minimum length we require from a discovery reply before we'll even try
/// to parse it. Matches the C# upstream threshold.
pub const DISCOVERY_REPLY_MIN_LEN: usize = 24;

/// HPSDR board types reported in byte `[10]` of a P1 discovery reply.
///
/// Taken from
/// `thetis-upstream/Project Files/Source/ChannelMaster/network.h:421-444`
/// and `Console/HPSDR/clsRadioDiscovery.cs::mapP1DeviceType`. Any unknown
/// value is preserved in [`HpsdrModel::Unknown`] so we don't silently
/// reject forward-compatible hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpsdrModel {
    Atlas,
    Hermes,
    HermesII,
    Angelia,
    Orion,
    /// HermesLite 2 — the primary phase-A test target.
    HermesLite,
    OrionMkII,
    Unknown(u8),
}

impl HpsdrModel {
    pub fn from_wire(byte: u8) -> Self {
        match byte {
            0  => HpsdrModel::Atlas,
            1  => HpsdrModel::Hermes,
            2  => HpsdrModel::HermesII,
            4  => HpsdrModel::Angelia,
            5  => HpsdrModel::Orion,
            6  => HpsdrModel::HermesLite,
            10 => HpsdrModel::OrionMkII,
            other => HpsdrModel::Unknown(other),
        }
    }

    pub fn to_wire(self) -> u8 {
        match self {
            HpsdrModel::Atlas        => 0,
            HpsdrModel::Hermes       => 1,
            HpsdrModel::HermesII     => 2,
            HpsdrModel::Angelia      => 4,
            HpsdrModel::Orion        => 5,
            HpsdrModel::HermesLite   => 6,
            HpsdrModel::OrionMkII    => 10,
            HpsdrModel::Unknown(b)   => b,
        }
    }
}

/// The discovery request sent to UDP `broadcast:1024`.
///
/// Stateless — the packet has no payload beyond the three magic bytes, so
/// this type is just a zero-sized marker. Use [`DiscoveryRequest::encode`]
/// to materialise the bytes to send.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscoveryRequest;

impl DiscoveryRequest {
    pub fn encode(self) -> [u8; DISCOVERY_REQUEST_LEN] {
        let mut buf = [0u8; DISCOVERY_REQUEST_LEN];
        buf[0] = 0xEF;
        buf[1] = 0xFE;
        buf[2] = 0x02;
        buf
    }
}

/// A parsed Protocol 1 discovery reply. All fields are optional where the
/// firmware being probed might be too old to populate them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryReply {
    /// Radio reported itself as idle (`0x02`) vs. already serving another
    /// client (`0x03`).
    pub busy: bool,
    /// 48-bit MAC address bytes in network order.
    pub mac: [u8; 6],
    /// Board model (byte `[10]`).
    pub model: HpsdrModel,
    /// Firmware code version (byte `[9]`).
    pub code_version: u8,
    /// Mercury board versions `[14..18]`, only set when `len > 20`.
    pub mercury_version: Option<[u8; 4]>,
    /// Penny board version `[18]`, only set when `len > 20`.
    pub penny_version: Option<u8>,
    /// Metis firmware version `[19]`, only set when `len > 20`.
    pub metis_version: Option<u8>,
    /// Number of receivers advertised by the radio `[20]`, only set when
    /// `len > 20`.
    pub num_rxs: Option<u8>,
}

impl DiscoveryReply {
    /// Attempt to parse a UDP payload as a Protocol 1 discovery reply.
    ///
    /// Returns `Ok(None)` if the packet is the right size but is clearly a
    /// Protocol 2 reply (which starts with `0x00 0x00 0x00 0x00`) — the
    /// caller can treat that as "not my packet" rather than an error.
    pub fn parse(data: &[u8]) -> Result<Option<Self>, ProtocolError> {
        if data.len() < DISCOVERY_REPLY_MIN_LEN {
            return Err(ProtocolError::Truncated {
                expected: DISCOVERY_REPLY_MIN_LEN,
                got: data.len(),
            });
        }

        // Protocol 2 replies start with four zero bytes then the status. If
        // we see that, say "not mine" so the caller can dispatch.
        if data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 0 {
            return Ok(None);
        }

        if data[0] != 0xEF || data[1] != 0xFE {
            return Err(ProtocolError::WrongMagic {
                found: [data[0], data[1]],
            });
        }

        let busy = match data[2] {
            0x02 => false,
            0x03 => true,
            other => return Err(ProtocolError::UnknownDiscoveryStatus(other)),
        };

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&data[3..9]);
        let code_version = data[9];
        let model = HpsdrModel::from_wire(data[10]);

        let (mercury_version, penny_version, metis_version, num_rxs) = if data.len() > 20 {
            (
                Some([data[14], data[15], data[16], data[17]]),
                Some(data[18]),
                Some(data[19]),
                Some(data[20]),
            )
        } else {
            (None, None, None, None)
        };

        Ok(Some(DiscoveryReply {
            busy,
            mac,
            model,
            code_version,
            mercury_version,
            penny_version,
            metis_version,
            num_rxs,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout_matches_upstream() {
        let pkt = DiscoveryRequest.encode();
        assert_eq!(pkt.len(), 63);
        assert_eq!(pkt[0], 0xEF);
        assert_eq!(pkt[1], 0xFE);
        assert_eq!(pkt[2], 0x02);
        // Everything else must be zero.
        assert!(pkt[3..].iter().all(|&b| b == 0));
    }

    #[test]
    fn reply_parses_idle_hermeslite() {
        let mut buf = [0u8; 60];
        buf[0] = 0xEF;
        buf[1] = 0xFE;
        buf[2] = 0x02; // idle
        buf[3..9].copy_from_slice(&[0x00, 0x1C, 0xC0, 0xA8, 0x01, 0x42]);
        buf[9]  = 0x49; // code version
        buf[10] = 6;    // HermesLite
        buf[14] = 0;
        buf[18] = 0;
        buf[19] = 0;
        buf[20] = 1;

        let reply = DiscoveryReply::parse(&buf).unwrap().unwrap();
        assert!(!reply.busy);
        assert_eq!(reply.model, HpsdrModel::HermesLite);
        assert_eq!(reply.code_version, 0x49);
        assert_eq!(reply.mac, [0x00, 0x1C, 0xC0, 0xA8, 0x01, 0x42]);
        assert_eq!(reply.num_rxs, Some(1));
    }

    #[test]
    fn reply_detects_busy_flag() {
        let mut buf = [0u8; 60];
        buf[0] = 0xEF;
        buf[1] = 0xFE;
        buf[2] = 0x03; // busy
        buf[10] = 1;   // Hermes
        let reply = DiscoveryReply::parse(&buf).unwrap().unwrap();
        assert!(reply.busy);
        assert_eq!(reply.model, HpsdrModel::Hermes);
    }

    #[test]
    fn reply_recognises_protocol2_and_returns_none() {
        let mut buf = [0u8; 60];
        // P2: first four bytes are zero, status at [4].
        buf[4] = 0x02;
        assert!(DiscoveryReply::parse(&buf).unwrap().is_none());
    }

    #[test]
    fn reply_rejects_truncated_packet() {
        let buf = [0u8; 8];
        matches!(
            DiscoveryReply::parse(&buf),
            Err(ProtocolError::Truncated { .. })
        );
    }

    #[test]
    fn model_roundtrip() {
        for byte in [0u8, 1, 2, 4, 5, 6, 10, 7, 42, 255] {
            let m = HpsdrModel::from_wire(byte);
            assert_eq!(m.to_wire(), byte, "model {m:?} wire byte {byte}");
        }
    }
}
