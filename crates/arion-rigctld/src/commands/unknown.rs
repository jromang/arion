//! Fallback for verbs the server doesn't recognise. Returns RPRT -11
//! (`RIG_ENAVAIL` — function not available) matching Hamlib's dummy.

use arion_app::App;

use super::RigCommand;
use crate::reply::RigReply;

#[derive(Debug)]
pub struct UnknownCmd;

impl RigCommand for UnknownCmd {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::Error(-11)
    }
}
