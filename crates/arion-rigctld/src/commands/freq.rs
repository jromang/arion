//! F / f — set/get VFO frequency.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetFreq {
    pub hz: u32,
}

impl RigCommand for SetFreq {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx() as u8;
        app.set_rx_frequency(rx, self.hz);
        RigReply::Ok
    }
}

#[derive(Debug)]
pub struct GetFreq;

impl RigCommand for GetFreq {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx();
        let hz = app.rx(rx).map(|r| r.frequency_hz).unwrap_or(0);
        RigReply::Value(format!("{hz}"))
    }
}

/// `F <hz>` — hamlib accepts integer or float Hz.
pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    let Some(raw) = args.first() else {
        return Box::new(UnknownCmd);
    };
    let hz: u32 = match raw.parse::<f64>() {
        Ok(f) if (0.0..=u32::MAX as f64).contains(&f) => f as u32,
        _ => return Box::new(UnknownCmd),
    };
    Box::new(SetFreq { hz })
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn set_then_get_freq() {
        let mut app = App::new(AppOptions::default());
        let _ = run("F 14074000", &mut app);
        let r = run("f", &mut app);
        match r {
            RigReply::Value(s) => assert_eq!(s, "14074000"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_freq_float_form() {
        let mut app = App::new(AppOptions::default());
        let _ = run("F 14074000.0", &mut app);
        let rx = app.active_rx();
        assert_eq!(app.rx(rx).unwrap().frequency_hz, 14_074_000);
    }
}
