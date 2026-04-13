//! Misc verbs: `\chk_vfo`, `\dump_state`, `q`/`\quit`.

use arion_app::App;

use super::RigCommand;
use crate::protocol::DUMP_STATE;
use crate::reply::RigReply;

#[derive(Debug)]
pub struct ChkVfo;

impl RigCommand for ChkVfo {
    fn execute(&self, _app: &mut App) -> RigReply {
        // `\chk_vfo` returns `CHKVFO 0` in the extended form; plain
        // mode clients accept a bare `0` line just as well. We emit
        // `0\n` followed by `RPRT 0` via the session.
        RigReply::Value("CHKVFO 0".into())
    }
}

#[derive(Debug)]
pub struct DumpState;

impl RigCommand for DumpState {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::Raw(DUMP_STATE.to_string())
    }
}

/// Sentinel for the session loop: the command is recognised, but the
/// session thread interprets `Ok` here as "close the connection now".
/// See `session.rs` for the sentinel check.
#[derive(Debug)]
pub struct Quit;

impl RigCommand for Quit {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::Ok
    }
}
