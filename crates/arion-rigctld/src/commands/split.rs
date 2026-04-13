//! S / s — split operation stub. Arion doesn't expose split TX yet;
//! accept the command and always report split=off.

use arion_app::App;

use super::{unknown::UnknownCmd, RigCommand};
use crate::reply::RigReply;

#[derive(Debug)]
pub struct SetSplit;

impl RigCommand for SetSplit {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::Ok
    }
}

#[derive(Debug)]
pub struct GetSplit;

impl RigCommand for GetSplit {
    fn execute(&self, _app: &mut App) -> RigReply {
        RigReply::KeyValues(vec![
            ("Split",   "0".into()),
            ("TX VFO",  "VFOA".into()),
        ])
    }
}

pub fn parse_set(args: &[&str]) -> Box<dyn RigCommand> {
    if args.is_empty() {
        Box::new(UnknownCmd)
    } else {
        Box::new(SetSplit)
    }
}

#[cfg(test)]
mod tests {
    use super::super::run;
    use crate::reply::RigReply;
    use arion_app::{App, AppOptions};

    #[test]
    fn get_split_reports_off() {
        let mut app = App::new(AppOptions::default());
        match run("s", &mut app) {
            RigReply::KeyValues(pairs) => assert_eq!(pairs[0].1, "0"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
