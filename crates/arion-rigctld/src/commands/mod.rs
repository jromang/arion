//! Command trait + dispatch parser.
//!
//! Each supported rigctld verb is implemented as a small struct in one
//! of the sibling `*.rs` files. The [`parse`] function matches on the
//! first token of a command line and boxes the appropriate struct.

use arion_app::App;

use crate::reply::RigReply;

pub mod freq;
pub mod level;
pub mod misc;
pub mod mode;
pub mod ptt;
pub mod split;
pub mod unknown;
pub mod vfo;

/// Trait every rigctld command implements.
///
/// `execute` runs on the UI thread (drained by `drain()`), so it has
/// exclusive mutable access to the `App` for the duration of the call.
pub trait RigCommand: Send + Sync + std::fmt::Debug {
    fn execute(&self, app: &mut App) -> RigReply;
}

/// Parse a command line (prefix-stripped of any extended `+`) into a
/// boxed command.
///
/// The dispatcher is intentionally flat: one arm per verb, each calling
/// a tiny `parse_*` helper that returns a `Box<dyn RigCommand>`. No
/// single giant match on implementation; the per-file `parse_*` does
/// the argument splitting.
pub fn parse(line: &str) -> Box<dyn RigCommand> {
    let mut it = line.split_whitespace();
    let Some(verb) = it.next() else {
        return Box::new(unknown::UnknownCmd);
    };
    let args: Vec<&str> = it.collect();
    match verb {
        "F" | "set_freq"         => freq::parse_set(&args),
        "f" | "get_freq"         => Box::new(freq::GetFreq),
        "M" | "set_mode"         => mode::parse_set(&args),
        "m" | "get_mode"         => Box::new(mode::GetMode),
        "V" | "set_vfo"          => vfo::parse_set(&args),
        "v" | "get_vfo"          => Box::new(vfo::GetVfo),
        "L" | "set_level"        => level::parse_set(&args),
        "l" | "get_level"        => level::parse_get(&args),
        "T" | "set_ptt"          => ptt::parse_set(&args),
        "t" | "get_ptt"          => Box::new(ptt::GetPtt),
        "S" | "set_split_vfo"    => split::parse_set(&args),
        "s" | "get_split_vfo"    => Box::new(split::GetSplit),
        "\\chk_vfo"              => Box::new(misc::ChkVfo),
        "\\dump_state"           => Box::new(misc::DumpState),
        "q" | "\\quit" | "Q"     => Box::new(misc::Quit),
        _ => Box::new(unknown::UnknownCmd),
    }
}

/// Convenience wrapper used in tests: run `line` against `app` and
/// return the resulting reply.
#[cfg(test)]
pub(crate) fn run(line: &str, app: &mut App) -> RigReply {
    parse(line).execute(app)
}
