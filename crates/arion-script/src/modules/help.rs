//! `help()` / `help("topic")` — returns a formatted string that the
//! REPL prints as the result of the expression. Because Rhai native
//! functions return a `Dynamic` which `run_line` already renders with
//! `{val}`, simply returning the help string gets it into the output
//! buffer without any need for a side-channel.

use rhai::Engine;

use crate::ctx::ApiCtx;
use crate::help_data::help_topics;
use crate::modules::ScriptModule;

pub struct HelpModule;

impl ScriptModule for HelpModule {
    fn register(&self, engine: &mut Engine, _ctx: &ApiCtx) {
        engine.register_fn("help", || -> String {
            let topics = help_topics();
            let mut keys: Vec<&&'static str> = topics.keys().collect();
            keys.sort();
            let overview = topics.get("overview").copied().unwrap_or("");
            let mut out = String::from(overview);
            out.push_str("\n\nTopics: ");
            let joined: Vec<&str> = keys.iter().map(|k| **k).collect();
            out.push_str(&joined.join(", "));
            out
        });

        engine.register_fn("help", |topic: &str| -> String {
            match help_topics().get(topic) {
                Some(s) => (*s).to_string(),
                None    => format!("no topic \"{topic}\". Type help() for the list."),
            }
        });
    }
}
