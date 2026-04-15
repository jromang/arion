//! Baudot (ITA2) 5-bit code ↔ ASCII conversion.
//!
//! Two shift states — LTRS (letters) and FIGS (figures) — are selected
//! by the reserved codes 31 and 27 respectively. The encoder inserts
//! shift codes as needed when the output character class changes.

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Shift {
    Letters,
    Figures,
}

/// LTRS code → ASCII, index = Baudot 5-bit value. Unused slots default
/// to space.
#[rustfmt::skip]
const LTRS: [u8; 32] = [
    b' ', b'E', b'\n', b'A', b' ', b'S', b'I', b'U',
    b'\r', b'D', b'R', b'J', b'N', b'F', b'C', b'K',
    b'T', b'Z', b'L', b'W', b'H', b'Y', b'P', b'Q',
    b'O', b'B', b'G', 0,    b'M', b'X', b'V', 0,
];

/// FIGS code → ASCII. Index 27 is FIGS shift, 31 is LTRS shift (both
/// markers, not emitted as characters).
#[rustfmt::skip]
const FIGS: [u8; 32] = [
    b' ', b'3', b'\n', b'-', b' ', b'\'', b'8', b'7',
    b'\r', b'$', b'4', b'\x07', b',', b'!', b':', b'(',
    b'5', b'"', b')', b'2', b'#', b'6', b'0', b'1',
    b'9', b'?', b'&', 0,    b'.', b'/', b';', 0,
];

pub const CODE_FIGS_SHIFT: u8 = 27;
pub const CODE_LTRS_SHIFT: u8 = 31;

/// Look up a decoded Baudot character. Returns `None` for shift codes
/// (which update state but produce no output).
pub fn decode(code: u8, shift: Shift) -> Option<u8> {
    match code {
        CODE_FIGS_SHIFT | CODE_LTRS_SHIFT => None,
        _ => {
            let c = match shift {
                Shift::Letters => LTRS[code as usize],
                Shift::Figures => FIGS[code as usize],
            };
            if c == 0 {
                None
            } else {
                Some(c)
            }
        }
    }
}

/// Look up the Baudot code for an ASCII byte, returning `(code,
/// shift_required)`. Returns `None` for characters that have no
/// Baudot representation. ASCII letters are upper-cased.
pub fn encode(c: u8) -> Option<(u8, Shift)> {
    let c = c.to_ascii_uppercase();
    if let Some(i) = LTRS.iter().position(|&b| b == c) {
        if i as u8 != CODE_FIGS_SHIFT && i as u8 != CODE_LTRS_SHIFT {
            return Some((i as u8, Shift::Letters));
        }
    }
    if let Some(i) = FIGS.iter().position(|&b| b == c) {
        if i as u8 != CODE_FIGS_SHIFT && i as u8 != CODE_LTRS_SHIFT {
            return Some((i as u8, Shift::Figures));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_round_trip() {
        for &c in b"HELLO WORLD" {
            let (code, sh) = encode(c).unwrap();
            assert_eq!(decode(code, sh), Some(c));
        }
    }

    #[test]
    fn figures_round_trip() {
        for &c in b"123 45 6789" {
            let (code, sh) = encode(c).unwrap();
            assert_eq!(decode(code, sh), Some(c));
        }
    }

    #[test]
    fn case_folded_to_upper() {
        let (code, sh) = encode(b'h').unwrap();
        assert_eq!(decode(code, sh), Some(b'H'));
    }
}
