//! Memories + band stack bindings.

use arion_settings::Memory;
use rhai::Engine;

use crate::ctx::ApiCtx;
use crate::modules::{mode_to_str, parse_mode, ScriptModule};
use super::{parse_band, rhai_err};

pub struct MemoryModule;

impl ScriptModule for MemoryModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx) {
        // memory_save(name) — snapshot the active RX into a new memory.
        let c = ctx.clone();
        engine.register_fn("memory_save", move |name: &str| {
            let name = name.to_string();
            let _ = c.with_app(|app| {
                let rx = app.active_rx();
                if let Some(r) = app.rx(rx) {
                    let mem = Memory {
                        name,
                        freq_hz: r.frequency_hz,
                        mode:    arion_app::mode_to_serde(r.mode),
                        tag:     String::new(),
                    };
                    app.add_memory(mem);
                }
            });
        });

        let c = ctx.clone();
        engine.register_fn("memory_load", move |idx: i64| {
            let _ = c.with_app(|app| app.load_memory(idx.max(0) as usize));
        });

        let c = ctx.clone();
        engine.register_fn("memory_delete", move |idx: i64| {
            let _ = c.with_app(|app| app.delete_memory(idx.max(0) as usize));
        });

        // band stack helpers (read-only view)
        let c = ctx.clone();
        engine.register_fn("band_stack_freq", move |b: &str| -> Result<i64, Box<rhai::EvalAltResult>> {
            let band = parse_band(b).map_err(rhai_err)?;
            let v = c.with_app(|app| app.band_stack().get(band).frequency_hz).map_err(rhai_err)?;
            Ok(v as i64)
        });
        let c = ctx.clone();
        engine.register_fn("band_stack_mode", move |b: &str| -> Result<String, Box<rhai::EvalAltResult>> {
            let band = parse_band(b).map_err(rhai_err)?;
            let v = c.with_app(|app| mode_to_str(app.band_stack().get(band).mode).to_string())
                .map_err(rhai_err)?;
            Ok(v)
        });

        // silence unused-import warning for `parse_mode` in small builds
        let _ = parse_mode;
    }
}
