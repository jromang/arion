//! Wire-level parsing + formatting of the rigctld protocol.
//!
//! This module translates between raw TCP bytes and strongly-typed
//! [`RigCommand`](crate::commands::RigCommand) trait objects on one side
//! and [`RigReply`](crate::reply::RigReply) on the other. The parser
//! dispatches on the first token; each arm calls a small `parse_*`
//! helper in one of the `commands/*.rs` files and boxes the struct.

use arion_core::WdspMode;

use crate::commands::{self, RigCommand};
use crate::reply::RigReply;

/// `\dump_state` body. Synthesised to look enough like Hamlib's `dummy`
/// rig output for WSJT-X / fldigi to accept it.
///
/// Values chosen from hamlib `tests/rigctl.c` + `dump_state` of the
/// dummy rig. The exact numbers here are mostly capability flags and
/// are not parsed strictly by clients — they check that *some* band
/// covering their TX freq is listed and that a mode it understands is
/// advertised.
pub const DUMP_STATE: &str = "\
0\n\
2\n\
2\n\
150000.000000 1500000000.000000 0x1ff -1 -1 0x10000003 0x3\n\
0 0 0 0 0 0 0\n\
150000.000000 1500000000.000000 0x1ff -1 -1 0x10000003 0x3\n\
0 0 0 0 0 0 0\n\
0 0\n\
0 0\n\
0\n\
0\n\
0\n\
0\n\
\n\
\n\
0x0\n\
0x0\n\
0x0\n\
0x0\n\
0x0\n\
0\n";

/// Parse a single line of client input into a boxed command.
///
/// Lines may start with `+` (extended response mode). The parser
/// strips it and returns `(cmd, extended)` so the session can format
/// the reply accordingly.
pub fn parse_line(line: &str) -> (Box<dyn RigCommand>, bool) {
    let trimmed = line.trim();
    let (body, extended) = if let Some(rest) = trimmed.strip_prefix('+') {
        (rest.trim_start(), true)
    } else {
        (trimmed, false)
    };

    let cmd = commands::parse(body);
    (cmd, extended)
}

/// Format a reply for the wire, honouring extended vs plain mode.
///
/// Plain mode: the last line is always `RPRT <n>`. `Value` / `KeyValues`
/// replies emit the values before the terminating `RPRT 0`.
///
/// Extended mode: the client echoes the command back on the first line
/// and expects `Key: Value` pairs, terminated by `RPRT <n>`. We emit a
/// minimal extended form that WSJT-X accepts: just the body, no echo
/// (WSJT-X tolerates missing echo), then `RPRT`.
pub fn format_reply(reply: &RigReply, extended: bool) -> String {
    match reply {
        RigReply::Ok => "RPRT 0\n".into(),
        RigReply::Error(n) => format!("RPRT {n}\n"),
        RigReply::Value(v) => format!("{v}\n"),
        RigReply::KeyValues(pairs) => {
            let mut out = String::new();
            for (k, v) in pairs {
                if extended {
                    out.push_str(&format!("{k}: {v}\n"));
                } else {
                    out.push_str(v);
                    out.push('\n');
                }
            }
            out
        }
        RigReply::Raw(s) => {
            let mut out = s.clone();
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    }
}

// --- Mode mapping --------------------------------------------------------

/// Convert an Arion [`WdspMode`] to a rigctld mode string.
///
/// `Spec` / `Drm` have no rigctld equivalent and are reported as USB.
pub fn mode_to_rigctld(mode: WdspMode) -> &'static str {
    match mode {
        WdspMode::Lsb  => "LSB",
        WdspMode::Usb  => "USB",
        WdspMode::CwL  => "CWR",
        WdspMode::CwU  => "CW",
        WdspMode::Am   => "AM",
        WdspMode::Sam  => "AMS",
        WdspMode::Fm   => "FM",
        WdspMode::DigL => "PKTLSB",
        WdspMode::DigU => "PKTUSB",
        WdspMode::Dsb  => "DSB",
        WdspMode::Spec => "USB",
        WdspMode::Drm  => "USB",
    }
}

pub fn parse_rigctld_mode(s: &str) -> Option<WdspMode> {
    match s.to_ascii_uppercase().as_str() {
        "LSB"              => Some(WdspMode::Lsb),
        "USB"              => Some(WdspMode::Usb),
        "CW"               => Some(WdspMode::CwU),
        "CWR"              => Some(WdspMode::CwL),
        "AM"               => Some(WdspMode::Am),
        "AMS" | "SAM"      => Some(WdspMode::Sam),
        "FM" | "WFM"       => Some(WdspMode::Fm),
        "PKTLSB" | "DIGL"  => Some(WdspMode::DigL),
        "PKTUSB" | "DIGU"  => Some(WdspMode::DigU),
        "DSB"              => Some(WdspMode::Dsb),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trip() {
        for m in [
            WdspMode::Lsb, WdspMode::Usb, WdspMode::CwL, WdspMode::CwU,
            WdspMode::Am, WdspMode::Sam, WdspMode::Fm, WdspMode::DigL,
            WdspMode::DigU, WdspMode::Dsb,
        ] {
            let s = mode_to_rigctld(m);
            let back = parse_rigctld_mode(s).unwrap();
            assert_eq!(back, m, "mode {m:?} did not round-trip");
        }
    }

    #[test]
    fn extended_prefix_stripped() {
        let (_cmd, ext) = parse_line("+f");
        assert!(ext);
        let (_cmd, ext2) = parse_line("f");
        assert!(!ext2);
    }

    #[test]
    fn format_ok_plain() {
        assert_eq!(format_reply(&RigReply::Ok, false), "RPRT 0\n");
    }

    #[test]
    fn format_error_plain() {
        assert_eq!(format_reply(&RigReply::Error(-11), false), "RPRT -11\n");
    }

    #[test]
    fn format_value_plain() {
        assert_eq!(format_reply(&RigReply::Value("14074000".into()), false), "14074000\n");
    }
}
