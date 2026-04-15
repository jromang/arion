//! AX.25 framing + HDLC bit-stuffing + CRC-16-CCITT (APRS flavour).
//!
//! Wire format (UI frame, no digipeaters):
//!
//! ```text
//!   7E  | dest 7 | src 7 (last-addr bit set) | 0x03 | 0xF0 | info... | FCS 2 | 7E
//! ```
//!
//! FCS = CRC-16 CCITT with polynomial 0x1021, initial 0xFFFF, XOR-out
//! 0xFFFF, transmitted little-endian (low byte first, LSB first inside
//! bytes — same as the rest of AX.25).

const CONTROL_UI: u8 = 0x03;
const PID_NO_L3: u8 = 0xF0;

#[derive(Debug, Clone)]
pub struct UiFrame {
    pub dest: Callsign,
    pub src: Callsign,
    pub info: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callsign {
    pub call: [u8; 6],
    pub ssid: u8,
}

impl Callsign {
    pub fn new(call: &str, ssid: u8) -> Self {
        let mut buf = [b' '; 6];
        for (i, b) in call.bytes().take(6).enumerate() {
            buf[i] = b.to_ascii_uppercase();
        }
        Self { call: buf, ssid: ssid & 0x0F }
    }

    fn as_str(&self) -> String {
        let s = std::str::from_utf8(&self.call).unwrap_or("").trim();
        if self.ssid == 0 {
            s.to_string()
        } else {
            format!("{s}-{}", self.ssid)
        }
    }
}

/// Encode an AX.25 callsign as 7 bytes. `last` sets the SSID byte's
/// last-address bit (bit 0 is zero for intermediate addresses).
fn encode_callsign(c: &Callsign, last: bool) -> [u8; 7] {
    let mut out = [0u8; 7];
    for (dst, &src) in out.iter_mut().zip(c.call.iter()).take(6) {
        *dst = src << 1;
    }
    // SSID byte layout: CRRSSSSL — C=command/response, R=reserved=1,
    // SSID in bits 1..=4, L=last-addr flag. APRS sends C=0, R=11.
    out[6] = 0b0110_0000 | ((c.ssid & 0x0F) << 1) | u8::from(last);
    out
}

fn decode_callsign(bytes: &[u8; 7]) -> Option<Callsign> {
    let mut call = [b' '; 6];
    for i in 0..6 {
        let c = bytes[i] >> 1;
        if c == 0 {
            return None;
        }
        call[i] = c;
    }
    let ssid = (bytes[6] >> 1) & 0x0F;
    Some(Callsign { call, ssid })
}

pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        let mut x = b;
        for _ in 0..8 {
            let bit = ((crc & 0x0001) != 0) ^ ((x & 0x01) != 0);
            crc >>= 1;
            if bit {
                crc ^= 0x8408; // reversed 0x1021
            }
            x >>= 1;
        }
    }
    crc ^ 0xFFFF
}

/// Serialize a `UiFrame` into the raw bytes between flags (addresses
/// + control + pid + info + FCS).
pub fn serialize_ui(frame: &UiFrame) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + frame.info.len() + 2);
    bytes.extend_from_slice(&encode_callsign(&frame.dest, false));
    bytes.extend_from_slice(&encode_callsign(&frame.src, true));
    bytes.push(CONTROL_UI);
    bytes.push(PID_NO_L3);
    bytes.extend_from_slice(&frame.info);
    let fcs = crc16_ccitt(&bytes);
    bytes.push((fcs & 0xFF) as u8);
    bytes.push((fcs >> 8) as u8);
    bytes
}

/// Parse a `UiFrame` from the raw bytes between flags. Returns `None`
/// on structural failure or CRC mismatch.
pub fn parse_ui(bytes: &[u8]) -> Option<UiFrame> {
    if bytes.len() < 16 {
        return None;
    }
    let (body, fcs_bytes) = bytes.split_at(bytes.len() - 2);
    let rx_fcs = u16::from(fcs_bytes[0]) | (u16::from(fcs_bytes[1]) << 8);
    if crc16_ccitt(body) != rx_fcs {
        return None;
    }
    let dest = decode_callsign(body[0..7].try_into().ok()?)?;
    let src = decode_callsign(body[7..14].try_into().ok()?)?;
    // We only parse simple 2-address UI frames; digipeater extension
    // (marked by unset last-addr on src) is rejected today.
    if (body[13] & 0x01) == 0 {
        return None;
    }
    if body[14] != CONTROL_UI || body[15] != PID_NO_L3 {
        return None;
    }
    let info = body[16..].to_vec();
    Some(UiFrame { dest, src, info })
}

/// HDLC bit-stuff: after any run of 5 consecutive `1` bits, insert a
/// `0` so the 6-bit flag pattern only appears in frame delimiters.
/// Input bit order per byte is LSB-first (AX.25 wire convention).
pub fn bit_stuff(bytes: &[u8]) -> Vec<bool> {
    let mut out = Vec::with_capacity(bytes.len() * 9);
    let mut ones = 0u8;
    for &b in bytes {
        for i in 0..8 {
            let bit = (b >> i) & 1 == 1;
            out.push(bit);
            if bit {
                ones += 1;
                if ones == 5 {
                    out.push(false);
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    out
}

/// HDLC un-stuff: strip a `0` after every run of 5 `1`s. A run of
/// exactly 6 `1`s is a flag (0x7E between bits); 7 or more is an
/// abort. `bits` should not include the enclosing flags.
pub fn bit_unstuff(bits: &[bool]) -> Option<Vec<bool>> {
    let mut out = Vec::with_capacity(bits.len());
    let mut ones = 0u8;
    let mut it = bits.iter();
    while let Some(&bit) = it.next() {
        out.push(bit);
        if bit {
            ones += 1;
            if ones == 5 {
                let &next = it.next()?;
                if next {
                    // Six consecutive 1s inside the payload is a flag
                    // or abort — the caller framed incorrectly.
                    return None;
                }
                ones = 0;
            }
        } else {
            ones = 0;
        }
    }
    Some(out)
}

/// Pack a bit stream back to bytes (LSB first).
pub fn bits_to_bytes(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        if b {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// Build the complete HDLC bit stream for a UI frame, including a
/// configurable number of leading and trailing flag bytes.
///
/// The transmitted stream is: `[flag]·lead · stuff(frame + FCS) ·
/// [flag]·trail`. Flags are 0x7E = 01111110 (LSB-first: 0,1,1,1,1,1,1,0).
/// They are never bit-stuffed; the 6-consecutive-1s pattern is the
/// receiver's flag detector.
pub fn frame_bits(frame: &UiFrame, leading_flags: usize, trailing_flags: usize) -> Vec<bool> {
    let payload = serialize_ui(frame);
    let stuffed = bit_stuff(&payload);
    let flag_bits = [false, true, true, true, true, true, true, false];
    let mut out = Vec::with_capacity(leading_flags * 8 + stuffed.len() + trailing_flags * 8);
    for _ in 0..leading_flags {
        out.extend_from_slice(&flag_bits);
    }
    out.extend_from_slice(&stuffed);
    for _ in 0..trailing_flags {
        out.extend_from_slice(&flag_bits);
    }
    out
}

/// Scan a raw bit stream (post-NRZI, pre-destuff) for an AX.25 UI
/// frame. Returns the first successfully-parsed frame found.
pub fn scan_bits_for_ui(raw: &[bool]) -> Option<UiFrame> {
    // Locate flag patterns: 01111110 LSB-first. We walk bit-by-bit,
    // collecting runs into frames between adjacent flags.
    let mut out: Option<UiFrame> = None;
    let flag: [bool; 8] = [false, true, true, true, true, true, true, false];
    let mut i = 0;
    while i + 8 <= raw.len() {
        if raw[i..i + 8] == flag {
            // Skip any back-to-back flags.
            let start = i + 8;
            let mut j = start;
            while j + 8 <= raw.len() && raw[j..j + 8] == flag {
                j += 8;
            }
            // Find the next flag.
            let body_start = j;
            while j + 8 <= raw.len() && raw[j..j + 8] != flag {
                j += 1;
            }
            if j + 8 > raw.len() {
                break;
            }
            let body = &raw[body_start..j];
            if let Some(unstuffed) = bit_unstuff(body) {
                // AX.25 expects whole-byte payloads.
                if unstuffed.len() % 8 == 0 {
                    let bytes = bits_to_bytes(&unstuffed);
                    if let Some(frame) = parse_ui(&bytes) {
                        out = Some(frame);
                        break;
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

impl UiFrame {
    /// Short human-readable header: `SRC>DEST`.
    pub fn header(&self) -> String {
        format!("{}>{}", self.src.as_str(), self.dest.as_str())
    }
    pub fn info_str(&self) -> String {
        String::from_utf8_lossy(&self.info).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> UiFrame {
        UiFrame {
            dest: Callsign::new("APZ000", 0),
            src: Callsign::new("F4XYZ", 0),
            info: b">hello aprs".to_vec(),
        }
    }

    #[test]
    fn crc_round_trip() {
        let bytes = serialize_ui(&sample_frame());
        let parsed = parse_ui(&bytes).unwrap();
        assert_eq!(parsed.info, b">hello aprs");
        assert_eq!(parsed.src.as_str(), "F4XYZ");
        assert_eq!(parsed.dest.as_str(), "APZ000");
    }

    #[test]
    fn bit_stuff_inserts_zero_after_five_ones() {
        // 0x1F LSB-first = 1,1,1,1,1,0,0,0. HDLC stuffs immediately
        // after the 5th `1` regardless of the following bit, so we
        // expect 9 output bits with a zero at position 5.
        let a = bit_stuff(&[0x1F]);
        assert_eq!(a.len(), 9);
        assert!(!a[5]);
        // 0xFF = all ones: same stuff point.
        let b = bit_stuff(&[0xFF]);
        assert_eq!(b.len(), 9);
        assert!(!b[5]);
    }

    #[test]
    fn bit_stuff_unstuff_round_trip() {
        let input = vec![0x7E, 0xFF, 0x00, 0xAA, 0x55, 0xFF, 0xFF];
        let stuffed = bit_stuff(&input);
        let unstuffed = bit_unstuff(&stuffed).unwrap();
        let bytes = bits_to_bytes(&unstuffed);
        assert_eq!(bytes, input);
    }

    #[test]
    fn scan_bits_finds_frame() {
        let bits = frame_bits(&sample_frame(), 4, 4);
        let parsed = scan_bits_for_ui(&bits).unwrap();
        assert_eq!(parsed.info_str(), ">hello aprs");
        assert_eq!(parsed.header(), "F4XYZ>APZ000");
    }
}
