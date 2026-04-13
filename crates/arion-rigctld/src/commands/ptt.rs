//! T / t — set/get PTT state. Arion has no TX path yet, so these are
//! stubs that accept any value, always report 0 (RX), and never fail
//! so WSJT-X doesn't refuse to connect.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetPtt {
    pub on: bool,
}

impl RigCommand for SetPtt {
    fn execute(&self, _app: &mut App) -> RigReply {
        // No TX path. Acknowledge so the client doesn't fail-fast.
        RigReply::Ok
    }
}

#[derive(Debug)]
pub struct GetPtt;

impl RigCommand for GetPtt {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::Value("0".into())
    }
}

pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    let Some(raw) = args.first() else {
        return Box::new(UnknownCmd);
    };
    let on = matches!(*raw, "1" | "TX" | "tx");
    Box::new(SetPtt { on })
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn get_ptt_is_zero() {
        let mut app = App::new(AppOptions::default());
        match run("t", &mut app) {
            RigReply::Value(v) => assert_eq!(v, "0"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_ptt_accepts() {
        let mut app = App::new(AppOptions::default());
        assert!(matches!(run("T 0", &mut app), RigReply::Ok));
        assert!(matches!(run("T 1", &mut app), RigReply::Ok));
    }
}
