//! Window show/hide/toggle bindings.
//!
//! The free-function `window(kind, open)` is registered by
//! [`RadioModule`](super::radio::RadioModule) so it stays colocated
//! with the other global REPL helpers. This module adds `toggle_window`
//! and the query helper `window_open`.

use rhai::Engine;

use crate::ctx::ApiCtx;
use crate::modules::{parse_window, rhai_err, ScriptModule};

pub struct WindowModule;

impl ScriptModule for WindowModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx) {
        let c = ctx.clone();
        engine.register_fn("toggle_window", move |kind: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let w = parse_window(kind).map_err(rhai_err)?;
            c.with_app(|app| app.toggle_window(w)).map_err(rhai_err)?;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("window_open", move |kind: &str| -> Result<bool, Box<rhai::EvalAltResult>> {
            let w = parse_window(kind).map_err(rhai_err)?;
            let v = c.with_app(|app| app.window_open(w)).map_err(rhai_err)?;
            Ok(v)
        });
    }
}
