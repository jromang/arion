//! PSK31 varicode decoder.
//!
//! Varicode is a self-synchronizing prefix-free code: each character
//! is a variable-length bit pattern with no internal "00" sequence;
//! two consecutive zeros "00" mark a character boundary.
//!
//! Reference: G3PLX's original PSK31 spec (1998), also documented at
//! <https://en.wikipedia.org/wiki/Varicode>.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Official varicode table (index = ASCII code, value = bit string
/// as an ASCII "1"/"0" literal). Source: G3PLX 1998 spec.
const VARICODE: &[(u8, &str)] = &[
    (0, "1010101011"),
    (1, "1011011011"),
    (2, "1011101101"),
    (3, "1101110111"),
    (4, "1011101011"),
    (5, "1101011111"),
    (6, "1011101111"),
    (7, "1011111101"),
    (8, "1011111111"),
    (9, "11101111"),
    (10, "11101"),
    (11, "1101101111"),
    (12, "1011011101"),
    (13, "11111"),
    (14, "1101110101"),
    (15, "1110101011"),
    (16, "1011110111"),
    (17, "1011110101"),
    (18, "1110101101"),
    (19, "1110101111"),
    (20, "1101011011"),
    (21, "1101101011"),
    (22, "1101101101"),
    (23, "1101010111"),
    (24, "1101111011"),
    (25, "1101111101"),
    (26, "1110110111"),
    (27, "1101010101"),
    (28, "1101011101"),
    (29, "1110111011"),
    (30, "1011111011"),
    (31, "1101111111"),
    (b' ', "1"),
    (b'!', "111111111"),
    (b'"', "101011111"),
    (b'#', "111110101"),
    (b'$', "111011011"),
    (b'%', "1011010101"),
    (b'&', "1010111011"),
    (b'\'', "101111111"),
    (b'(', "11111011"),
    (b')', "11110111"),
    (b'*', "101101111"),
    (b'+', "111011111"),
    (b',', "1110101"),
    (b'-', "110101"),
    (b'.', "1010111"),
    (b'/', "110101111"),
    (b'0', "10110111"),
    (b'1', "10111101"),
    (b'2', "11101101"),
    (b'3', "11111111"),
    (b'4', "101110111"),
    (b'5', "101011011"),
    (b'6', "101101011"),
    (b'7', "110101101"),
    (b'8', "110101011"),
    (b'9', "110110111"),
    (b':', "11110101"),
    (b';', "110111101"),
    (b'<', "111101101"),
    (b'=', "1010101"),
    (b'>', "111010111"),
    (b'?', "1010101111"),
    (b'@', "1010111101"),
    (b'A', "1111101"),
    (b'B', "11101011"),
    (b'C', "10101101"),
    (b'D', "10110101"),
    (b'E', "1110111"),
    (b'F', "11011011"),
    (b'G', "11111101"),
    (b'H', "101010101"),
    (b'I', "1111111"),
    (b'J', "111111101"),
    (b'K', "101111101"),
    (b'L', "11010111"),
    (b'M', "10111011"),
    (b'N', "11011101"),
    (b'O', "10101011"),
    (b'P', "11010101"),
    (b'Q', "111011101"),
    (b'R', "10101111"),
    (b'S', "1101111"),
    (b'T', "1101101"),
    (b'U', "101010111"),
    (b'V', "110110101"),
    (b'W', "101011101"),
    (b'X', "101110101"),
    (b'Y', "101111011"),
    (b'Z', "1010101101"),
    (b'[', "111110111"),
    (b'\\', "111101111"),
    (b']', "111111011"),
    (b'^', "1010111111"),
    (b'_', "101101101"),
    (b'`', "1011011111"),
    (b'a', "1011"),
    (b'b', "1011111"),
    (b'c', "101111"),
    (b'd', "101101"),
    (b'e', "11"),
    (b'f', "111101"),
    (b'g', "1011011"),
    (b'h', "101011"),
    (b'i', "1101"),
    (b'j', "111101011"),
    (b'k', "10111111"),
    (b'l', "11011"),
    (b'm', "111011"),
    (b'n', "1111"),
    (b'o', "111"),
    (b'p', "111111"),
    (b'q', "110111111"),
    (b'r', "10101"),
    (b's', "10111"),
    (b't', "101"),
    (b'u', "110111"),
    (b'v', "1111011"),
    (b'w', "1101011"),
    (b'x', "11011111"),
    (b'y', "1011101"),
    (b'z', "111010101"),
    (b'{', "1010110111"),
    (b'|', "110111011"),
    (b'}', "1010110101"),
    (b'~', "1011010111"),
    (127, "1110110101"),
];

fn table() -> &'static HashMap<&'static str, u8> {
    static T: OnceLock<HashMap<&'static str, u8>> = OnceLock::new();
    T.get_or_init(|| VARICODE.iter().map(|&(c, s)| (s, c)).collect())
}

/// Look up the varicode bit-string for an ASCII byte. Returns `None`
/// for bytes outside the 0..128 table or with no assigned code.
pub fn code_for(c: u8) -> Option<&'static str> {
    VARICODE.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

/// Streaming varicode decoder. Feed bits one by one with `push_bit`;
/// decoded ASCII characters are returned from `drain`.
#[derive(Default, Debug)]
pub struct VaricodeDecoder {
    bits: String,
    zero_run: u8,
    out: Vec<u8>,
}

impl VaricodeDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bit(&mut self, bit: bool) {
        self.bits.push(if bit { '1' } else { '0' });
        if bit {
            self.zero_run = 0;
        } else {
            self.zero_run += 1;
        }

        if self.zero_run >= 2 {
            // Trim the terminator "00" and try to look up the code.
            let code = &self.bits[..self.bits.len() - 2];
            if !code.is_empty() {
                if let Some(&c) = table().get(code) {
                    self.out.push(c);
                }
            }
            self.bits.clear();
            self.zero_run = 0;
        }
    }

    pub fn drain(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.out)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_str_bits(d: &mut VaricodeDecoder, s: &str) {
        for b in s.bytes() {
            d.push_bit(b == b'1');
        }
    }

    #[test]
    fn single_space() {
        let mut d = VaricodeDecoder::new();
        push_str_bits(&mut d, "100");
        assert_eq!(d.drain(), " ");
    }

    #[test]
    fn word_hi() {
        let mut d = VaricodeDecoder::new();
        // "h" = 101011 + 00, "i" = 1101 + 00
        push_str_bits(&mut d, "10101100110100");
        assert_eq!(d.drain(), "hi");
    }

    #[test]
    fn lowercase_e_is_shortest() {
        let mut d = VaricodeDecoder::new();
        push_str_bits(&mut d, "1100");
        assert_eq!(d.drain(), "e");
    }

    #[test]
    fn unknown_code_ignored() {
        let mut d = VaricodeDecoder::new();
        push_str_bits(&mut d, "010101010100");
        assert_eq!(d.drain(), "");
    }
}
