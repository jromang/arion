//! V / v — set/get active VFO. Maps VFOA→RX1, VFOB→RX2.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetVfo {
    pub rx: usize,
}

impl RigCommand for SetVfo {
    fn execute(&self, app: &mut App) -> RigReply {
        app.set_active_rx(self.rx);
        RigReply::Ok
    }
}

#[derive(Debug)]
pub struct GetVfo;

impl RigCommand for GetVfo {
    fn execute(&self, app: &mut App) -> RigReply {
        let name = match app.active_rx() {
            0 => "VFOA",
            _ => "VFOB",
        };
        RigReply::Value(name.into())
    }
}

pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    let Some(raw) = args.first() else {
        return Box::new(UnknownCmd);
    };
    let rx = match raw.to_ascii_uppercase().as_str() {
        "VFOA" | "MAIN" | "A" => 0,
        "VFOB" | "SUB"  | "B" => 1,
        _ => return Box::new(UnknownCmd),
    };
    Box::new(SetVfo { rx })
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn set_vfo_b_flips_active() {
        let mut app = App::new(AppOptions::default());
        let _ = run("V VFOB", &mut app);
        assert_eq!(app.active_rx(), 1);
        match run("v", &mut app) {
            RigReply::Value(s) => assert_eq!(s, "VFOB"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
