//! M / m — set/get mode + passband width.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::protocol::{mode_to_rigctld, parse_rigctld_mode};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetMode {
    pub mode: arion_core::WdspMode,
    pub bw_hz: Option<i32>,
}

impl RigCommand for SetMode {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx() as u8;
        app.set_rx_mode(rx, self.mode);
        if let Some(bw) = self.bw_hz {
            if bw > 0 {
                let width = bw as f64;
                let mode = self.mode;
                let (lo, hi) = match mode {
                    arion_core::WdspMode::Usb | arion_core::WdspMode::DigU => (200.0, 200.0 + width),
                    arion_core::WdspMode::Lsb | arion_core::WdspMode::DigL => (-(200.0 + width), -200.0),
                    _ => (-width / 2.0, width / 2.0),
                };
                app.set_rx_filter(rx, lo, hi);
            }
        }
        RigReply::Ok
    }
}

#[derive(Debug)]
pub struct GetMode;

impl RigCommand for GetMode {
    fn execute(&self, app: &mut App) -> RigReply {
        let rx = app.active_rx();
        let (mode, bw) = match app.rx(rx) {
            Some(r) => (r.mode, (r.filter_hi - r.filter_lo).abs().round() as i32),
            None => (arion_core::WdspMode::Usb, 2400),
        };
        RigReply::KeyValues(vec![
            ("Mode",      mode_to_rigctld(mode).to_string()),
            ("Passband",  bw.to_string()),
        ])
    }
}

pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    let Some(mode_str) = args.first() else {
        return Box::new(UnknownCmd);
    };
    let Some(mode) = parse_rigctld_mode(mode_str) else {
        return Box::new(UnknownCmd);
    };
    let bw_hz = args.get(1).and_then(|s| s.parse::<i32>().ok());
    Box::new(SetMode { mode, bw_hz })
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn set_mode_usb() {
        let mut app = App::new(AppOptions::default());
        let _ = run("M USB 2400", &mut app);
        let rx = app.active_rx();
        assert_eq!(app.rx(rx).unwrap().mode, arion_core::WdspMode::Usb);
    }

    #[test]
    fn get_mode_returns_kv() {
        let mut app = App::new(AppOptions::default());
        let _ = run("M LSB 2700", &mut app);
        match run("m", &mut app) {
            RigReply::KeyValues(pairs) => {
                assert_eq!(pairs[0].1, "LSB");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
