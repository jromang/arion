//! `Radio` Rhai type + top-level free functions covering global
//! radio state (connect/disconnect, active RX, save, tick).

use rhai::Engine;

use crate::ctx::ApiCtx;
use crate::modules::{
    agc_to_str, mode_to_str, parse_agc, parse_band, parse_filter_preset, parse_mode,
    parse_window, rhai_err, ScriptModule,
};
use crate::modules::rx::Rx;

/// Singleton handle — zero state, every accessor reads live from `App`.
#[derive(Clone, Debug, Default)]
pub struct Radio;

pub struct RadioModule;

impl ScriptModule for RadioModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx) {
        engine.register_type_with_name::<Radio>("Radio");

        // --- Radio.connected (RO) ---
        let c = ctx.clone();
        engine.register_get("connected", move |_r: &mut Radio| -> bool {
            c.with_app(|app| app.is_connected()).unwrap_or(false)
        });

        // --- Radio.ip (RO) ---
        let c = ctx.clone();
        engine.register_get("ip", move |_r: &mut Radio| -> String {
            c.with_app(|app| app.radio_ip().to_string()).unwrap_or_default()
        });

        // --- Radio.num_rx (RO) ---
        let c = ctx.clone();
        engine.register_get("num_rx", move |_r: &mut Radio| -> i64 {
            c.with_app(|app| app.num_rx() as i64).unwrap_or(0)
        });

        // --- Radio.active_rx (RO) ---
        let c = ctx.clone();
        engine.register_get("active_rx", move |_r: &mut Radio| -> i64 {
            c.with_app(|app| app.active_rx() as i64).unwrap_or(0)
        });

        // --- Radio.last_error (RO) ---
        let c = ctx.clone();
        engine.register_get("last_error", move |_r: &mut Radio| -> String {
            c.with_app(|app| app.last_error().unwrap_or("").to_string()).unwrap_or_default()
        });

        // --- Radio.rx(i) as method (reads, method chaining) and as
        // indexer `radio[i]` (so property-setter chaining works —
        // Rhai's parser only allows assignment-LHS through indexers
        // and getters, not through function-call returns).
        engine.register_fn("rx", |_r: &mut Radio, i: i64| -> Rx {
            Rx::new(i.max(0) as u8)
        });
        engine.register_indexer_get(|_r: &mut Radio, i: i64| -> Rx {
            Rx::new(i.max(0) as u8)
        });

        // --- Methods on Radio ---
        let c = ctx.clone();
        engine.register_fn("connect", move |_r: &mut Radio| {
            let _ = c.with_app(|app| app.connect());
        });
        let c = ctx.clone();
        engine.register_fn("disconnect", move |_r: &mut Radio| {
            let _ = c.with_app(|app| app.disconnect());
        });
        let c = ctx.clone();
        engine.register_fn("save", move |_r: &mut Radio| {
            let _ = c.with_app(|app| app.save_now());
        });
        let c = ctx.clone();
        engine.register_fn("tick", move |_r: &mut Radio| {
            let _ = c.with_app(|app| app.tick(std::time::Instant::now()));
        });

        // --- Display ---
        engine.register_fn("to_string", |_r: &mut Radio| "Radio".to_string());

        // ---------------------------------------------------------------
        // Free functions (REPL ergonomics)
        // ---------------------------------------------------------------

        let c = ctx.clone();
        engine.register_fn("freq", move |rx: Rx, hz: i64| {
            let _ = c.with_app(|app| app.set_rx_frequency(rx.index, hz.max(0) as u32));
        });
        let c = ctx.clone();
        engine.register_fn("mode", move |rx: Rx, m: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let md = parse_mode(m).map_err(rhai_err)?;
            c.with_app(|app| app.set_rx_mode(rx.index, md)).map_err(rhai_err)?;
            Ok(())
        });
        let c = ctx.clone();
        engine.register_fn("volume", move |rx: Rx, v: f64| {
            let _ = c.with_app(|app| app.set_rx_volume(rx.index, v as f32));
        });
        let c = ctx.clone();
        engine.register_fn("mute", move |rx: Rx, on: bool| {
            let _ = c.with_app(|app| app.set_rx_muted(rx.index, on));
        });
        let c = ctx.clone();
        engine.register_fn("lock", move |rx: Rx, on: bool| {
            let _ = c.with_app(|app| app.set_rx_locked(rx.index, on));
        });
        let c = ctx.clone();
        engine.register_fn("rit", move |rx: Rx, hz: i64| {
            let _ = c.with_app(|app| app.set_rx_rit(rx.index, hz as i32));
        });

        // Flag toggles
        for (fname, flag) in [
            ("nr3",  "nr3"),
            ("nr4",  "nr4"),
            ("anr",  "anr"),
            ("emnr", "emnr"),
            ("nb",   "nb"),
            ("nb2",  "nb2"),
            ("anf",  "anf"),
            ("bin",  "bin"),
            ("tnf",  "tnf"),
        ] {
            let c = ctx.clone();
            let flg = flag.to_string();
            engine.register_fn(fname, move |rx: Rx, on: bool| {
                let flg = flg.clone();
                let _ = c.with_app(|app| match flg.as_str() {
                    "nr3"  => app.set_rx_nr3(rx.index, on),
                    "nr4"  => app.set_rx_nr4(rx.index, on),
                    "anr"  => app.set_rx_anr(rx.index, on),
                    "emnr" => app.set_rx_emnr(rx.index, on),
                    "nb" | "nb2" | "anf" | "bin" | "tnf" => {
                        let rxi = rx.index;
                        let cur = app.rx(rxi as usize).map(|r| match flg.as_str() {
                            "nb"  => r.nb,
                            "nb2" => r.nb2,
                            "anf" => r.anf,
                            "bin" => r.bin,
                            "tnf" => r.tnf,
                            _ => false,
                        }).unwrap_or(false);
                        if cur != on {
                            app.toggle_rx_flag(rxi, flg.as_str());
                        }
                    }
                    _ => {}
                });
            });
        }

        // Squelch
        let c = ctx.clone();
        engine.register_fn("squelch", move |rx: Rx, on: bool| {
            let _ = c.with_app(|app| app.set_rx_squelch(rx.index, on));
        });
        let c = ctx.clone();
        engine.register_fn("squelch_db", move |rx: Rx, db: f64| {
            let _ = c.with_app(|app| app.set_rx_squelch_threshold(rx.index, db as f32));
        });

        // APF
        let c = ctx.clone();
        engine.register_fn("apf", move |rx: Rx, on: bool| {
            let _ = c.with_app(|app| app.set_rx_apf(rx.index, on));
        });

        // CTCSS
        let c = ctx.clone();
        engine.register_fn("ctcss", move |rx: Rx, on: bool| {
            let _ = c.with_app(|app| app.set_rx_ctcss(rx.index, on));
        });
        let c = ctx.clone();
        engine.register_fn("ctcss_hz", move |rx: Rx, hz: f64| {
            let _ = c.with_app(|app| app.set_rx_ctcss_freq(rx.index, hz as f32));
        });

        // TNF notch CRUD
        let c = ctx.clone();
        engine.register_fn("tnf_add", move |rx: Rx, freq_hz: f64, width_hz: f64| {
            let _ = c.with_app(|app| app.add_rx_tnf_notch(rx.index, freq_hz, width_hz, true));
        });
        let c = ctx.clone();
        engine.register_fn("tnf_delete", move |rx: Rx, nidx: i64| {
            let _ = c.with_app(|app| app.delete_rx_tnf_notch(rx.index, nidx.max(0) as u32));
        });

        let c = ctx.clone();
        engine.register_fn("agc", move |rx: Rx, preset: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let a = parse_agc(preset).map_err(rhai_err)?;
            c.with_app(|app| app.set_rx_agc(rx.index, a)).map_err(rhai_err)?;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("filter", move |rx: Rx, lo: f64, hi: f64| {
            let _ = c.with_app(|app| app.set_rx_filter(rx.index, lo, hi));
        });
        let c = ctx.clone();
        engine.register_fn("filter_preset", move |rx: Rx, preset: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let p = parse_filter_preset(preset).map_err(rhai_err)?;
            c.with_app(|app| app.set_rx_filter_preset(rx.index, p)).map_err(rhai_err)?;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("eq", move |rx: Rx, arr: rhai::Array| -> Result<(), Box<rhai::EvalAltResult>> {
            if arr.len() != 11 {
                return Err(rhai_err(format!("eq requires 11 values, got {}", arr.len())));
            }
            let mut gains = [0i32; 11];
            for (i, v) in arr.iter().enumerate() {
                gains[i] = v.as_int().map_err(|t| rhai_err(format!("eq[{i}] not int, got {t}")))? as i32;
            }
            c.with_app(|app| app.set_rx_eq_gains(rx.index, gains)).map_err(rhai_err)?;
            Ok(())
        });
        let c = ctx.clone();
        engine.register_fn("eq_band", move |rx: Rx, i: i64, g: i64| {
            let _ = c.with_app(|app| app.set_rx_eq_band(rx.index, i.max(0) as usize, g as i32));
        });

        // band(b) — active RX
        let c = ctx.clone();
        engine.register_fn("band", move |b: &str| -> Result<(), Box<rhai::EvalAltResult>> {
            let band = parse_band(b).map_err(rhai_err)?;
            c.with_app(|app| app.jump_to_band(band)).map_err(rhai_err)?;
            Ok(())
        });
        // tune(hz) — active RX
        let c = ctx.clone();
        engine.register_fn("tune", move |hz: i64| {
            let _ = c.with_app(|app| {
                let rx = app.active_rx() as u8;
                app.set_rx_frequency(rx, hz.max(0) as u32);
            });
        });

        let c = ctx.clone();
        engine.register_fn("active_rx", move |i: i64| {
            let _ = c.with_app(|app| app.set_active_rx(i.max(0) as usize));
        });
        let c = ctx.clone();
        engine.register_fn("num_rx", move |n: i64| {
            let _ = c.with_app(|app| app.set_num_rx(n.clamp(1, 255) as u8));
        });

        // Connect / disconnect as free functions
        let c = ctx.clone();
        engine.register_fn("connect", move || {
            let _ = c.with_app(|app| app.connect());
        });
        let c = ctx.clone();
        engine.register_fn("disconnect", move || {
            let _ = c.with_app(|app| app.disconnect());
        });

        // window(kind, open)
        let c = ctx.clone();
        engine.register_fn("window", move |kind: &str, open: bool| -> Result<(), Box<rhai::EvalAltResult>> {
            let w = parse_window(kind).map_err(rhai_err)?;
            c.with_app(|app| app.set_window_open(w, open)).map_err(rhai_err)?;
            Ok(())
        });

        let c = ctx.clone();
        engine.register_fn("save", move || {
            let _ = c.with_app(|app| app.save_now());
        });
        let c = ctx.clone();
        engine.register_fn("audio_device", move |name: &str| {
            let n = name.to_string();
            let _ = c.with_app(|app| app.set_audio_device_name(n));
        });

        // --- String mode/agc helpers so scripts can stringify ---
        engine.register_fn("mode_str", |m: &str| -> String {
            parse_mode(m).map(|x| mode_to_str(x).to_string()).unwrap_or_else(|_| m.to_string())
        });
        engine.register_fn("agc_str", |a: &str| -> String {
            parse_agc(a).map(|x| agc_to_str(x).to_string()).unwrap_or_else(|_| a.to_string())
        });
    }
}
