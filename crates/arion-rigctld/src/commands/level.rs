//! L / l — set/get DSP level. Only `AF` (audio-frequency gain → RX
//! volume) is mapped; any other level returns RPRT -11.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetLevel {
    pub name: String,
    pub value: f32,
}

impl RigCommand for SetLevel {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx() as u8;
        match self.name.as_str() {
            "AF" => {
                app.set_rx_volume(rx, self.value.clamp(0.0, 1.0));
                RigReply::Ok
            }
            _ => RigReply::Error(-11),
        }
    }
}

#[derive(Debug)]
pub struct GetLevel {
    pub name: String,
}

impl RigCommand for GetLevel {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx();
        match self.name.as_str() {
            "AF" => {
                let v = app.rx(rx).map(|r| r.volume).unwrap_or(0.0);
                RigReply::Value(format!("{v:.6}"))
            }
            _ => RigReply::Error(-11),
        }
    }
}

pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    let (Some(name), Some(raw)) = (args.first(), args.get(1)) else {
        return Box::new(UnknownCmd);
    };
    let Ok(v) = raw.parse::<f32>() else {
        return Box::new(UnknownCmd);
    };
    Box::new(SetLevel { name: (*name).to_string(), value: v })
}

pub fn parse_get(args: &[&str]) -> Box<dyn RigCommand> {
    let Some(name) = args.first() else {
        return Box::new(UnknownCmd);
    };
    Box::new(GetLevel { name: (*name).to_string() })
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn set_af_sets_volume() {
        let mut app = App::new(AppOptions::default());
        let _ = run("L AF 0.4", &mut app);
        let rx = app.active_rx();
        assert!((app.rx(rx).unwrap().volume - 0.4).abs() < 1e-5);
    }

    #[test]
    fn unknown_level_errors() {
        let mut app = App::new(AppOptions::default());
        match run("L RFPOWER 0.5", &mut app) {
            RigReply::Error(-11) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}
