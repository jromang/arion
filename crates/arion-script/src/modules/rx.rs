//! `Rx` Rhai type: one navigable handle per receiver index.

use rhai::{Array, Dynamic, Engine};

use crate::ctx::ApiCtx;
use crate::modules::{
    agc_to_str, mode_to_str, parse_agc, parse_mode, rhai_err, ScriptModule,
};

/// Zero-state handle; the index is the only thing we carry around.
/// The actual state is always read live from `App`.
#[derive(Clone, Debug)]
pub struct Rx {
    pub index: u8,
}

impl Rx {
    pub fn new(index: u8) -> Self { Rx { index } }
}

pub struct RxModule;

impl ScriptModule for RxModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx) {
        engine.register_type_with_name::<Rx>("Rx");

        // --- freq (u32 Hz) ---
        let c = ctx.clone();
        engine.register_get("freq", move |rx: &mut Rx| -> i64 {
            c.with_app(|app| app.rx(rx.index as usize).map(|r| r.frequency_hz as i64).unwrap_or(0))
                .unwrap_or(0)
        });
        let c = ctx.clone();
        engine.register_set("freq", move |rx: &mut Rx, hz: i64| {
            let _ = c.with_app(|app| app.set_rx_frequency(rx.index, hz.max(0) as u32));
        });

        // --- mode ---
        let c = ctx.clone();
        engine.register_get("mode", move |rx: &mut Rx| -> String {
            c.with_app(|app| {
                app.rx(rx.index as usize).map(|r| mode_to_str(r.mode).to_string()).unwrap_or_default()
            })
            .unwrap_or_default()
        });
        let c = ctx.clone();
        engine.register_set("mode", move |rx: &mut Rx, s: String| -> Result<(), Box<rhai::EvalAltResult>> {
            let mode = parse_mode(&s).map_err(rhai_err)?;
            c.with_app(|app| app.set_rx_mode(rx.index, mode)).map_err(rhai_err)?;
            Ok(())
        });

        // --- volume (f32) ---
        let c = ctx.clone();
        engine.register_get("volume", move |rx: &mut Rx| -> f64 {
            c.with_app(|app| app.rx(rx.index as usize).map(|r| r.volume as f64).unwrap_or(0.0))
                .unwrap_or(0.0)
        });
        let c = ctx.clone();
        engine.register_set("volume", move |rx: &mut Rx, v: f64| {
            let _ = c.with_app(|app| app.set_rx_volume(rx.index, v as f32));
        });

        // --- muted / locked / enabled ---
        reg_bool_rx(engine, ctx, "muted",
            |app, i| app.rx(i).map(|r| r.muted).unwrap_or(false),
            |app, i, b| app.set_rx_muted(i, b),
        );
        reg_bool_rx(engine, ctx, "locked",
            |app, i| app.rx(i).map(|r| r.locked).unwrap_or(false),
            |app, i, b| app.set_rx_locked(i, b),
        );
        reg_bool_rx(engine, ctx, "enabled",
            |app, i| app.rx(i).map(|r| r.enabled).unwrap_or(false),
            |app, i, b| app.set_rx_enabled(i, b),
        );

        // --- filter_lo / filter_hi ---
        let c = ctx.clone();
        engine.register_get("filter_lo", move |rx: &mut Rx| -> f64 {
            c.with_app(|app| app.rx(rx.index as usize).map(|r| r.filter_lo).unwrap_or(0.0))
                .unwrap_or(0.0)
        });
        let c = ctx.clone();
        engine.register_set("filter_lo", move |rx: &mut Rx, v: f64| {
            let _ = c.with_app(|app| {
                if let Some(r) = app.rx(rx.index as usize) {
                    let hi = r.filter_hi;
                    app.set_rx_filter(rx.index, v, hi);
                }
            });
        });
        let c = ctx.clone();
        engine.register_get("filter_hi", move |rx: &mut Rx| -> f64 {
            c.with_app(|app| app.rx(rx.index as usize).map(|r| r.filter_hi).unwrap_or(0.0))
                .unwrap_or(0.0)
        });
        let c = ctx.clone();
        engine.register_set("filter_hi", move |rx: &mut Rx, v: f64| {
            let _ = c.with_app(|app| {
                if let Some(r) = app.rx(rx.index as usize) {
                    let lo = r.filter_lo;
                    app.set_rx_filter(rx.index, lo, v);
                }
            });
        });

        // --- noise reduction booleans (4 independent variants) ---
        reg_bool_rx(engine, ctx, "nr3",
            |app, i| app.rx(i).map(|r| r.nr3).unwrap_or(false),
            |app, i, b| app.set_rx_nr3(i, b),
        );
        reg_bool_rx(engine, ctx, "nr4",
            |app, i| app.rx(i).map(|r| r.nr4).unwrap_or(false),
            |app, i, b| app.set_rx_nr4(i, b),
        );
        reg_bool_rx(engine, ctx, "anr",
            |app, i| app.rx(i).map(|r| r.anr).unwrap_or(false),
            |app, i, b| app.set_rx_anr(i, b),
        );
        reg_bool_rx(engine, ctx, "emnr",
            |app, i| app.rx(i).map(|r| r.emnr).unwrap_or(false),
            |app, i, b| app.set_rx_emnr(i, b),
        );

        // --- squelch (on/off + threshold) ---
        reg_bool_rx(engine, ctx, "squelch",
            |app, i| app.rx(i).map(|r| r.squelch).unwrap_or(false),
            |app, i, b| app.set_rx_squelch(i, b),
        );
        reg_f32_rx(engine, ctx, "squelch_db",
            |app, i| app.rx(i).map(|r| r.squelch_db).unwrap_or(0.0),
            |app, i, v| app.set_rx_squelch_threshold(i, v),
        );

        // --- APF (CW audio peak filter) ---
        reg_bool_rx(engine, ctx, "apf",
            |app, i| app.rx(i).map(|r| r.apf).unwrap_or(false),
            |app, i, b| app.set_rx_apf(i, b),
        );
        reg_f32_rx(engine, ctx, "apf_freq_hz",
            |app, i| app.rx(i).map(|r| r.apf_freq_hz).unwrap_or(0.0),
            |app, i, v| app.set_rx_apf_freq(i, v),
        );
        reg_f32_rx(engine, ctx, "apf_bw_hz",
            |app, i| app.rx(i).map(|r| r.apf_bw_hz).unwrap_or(0.0),
            |app, i, v| app.set_rx_apf_bandwidth(i, v),
        );
        reg_f32_rx(engine, ctx, "apf_gain_db",
            |app, i| app.rx(i).map(|r| r.apf_gain_db).unwrap_or(0.0),
            |app, i, v| app.set_rx_apf_gain(i, v),
        );

        // --- AGC fine controls ---
        reg_f32_rx(engine, ctx, "agc_top_dbm",
            |app, i| app.rx(i).map(|r| r.agc_top_dbm).unwrap_or(0.0),
            |app, i, v| app.set_rx_agc_top(i, v),
        );
        reg_f32_rx(engine, ctx, "agc_hang_level",
            |app, i| app.rx(i).map(|r| r.agc_hang_level).unwrap_or(0.0),
            |app, i, v| app.set_rx_agc_hang_level(i, v),
        );
        reg_i32_rx(engine, ctx, "agc_decay_ms",
            |app, i| app.rx(i).map(|r| r.agc_decay_ms).unwrap_or(0),
            |app, i, v| app.set_rx_agc_decay(i, v),
        );
        reg_f32_rx(engine, ctx, "agc_fixed_gain",
            |app, i| app.rx(i).map(|r| r.agc_fixed_gain).unwrap_or(0.0),
            |app, i, v| app.set_rx_agc_fixed_gain(i, v),
        );

        // --- FM parameters + CTCSS ---
        reg_f32_rx(engine, ctx, "fm_deviation_hz",
            |app, i| app.rx(i).map(|r| r.fm_deviation_hz).unwrap_or(0.0),
            |app, i, v| app.set_rx_fm_deviation(i, v),
        );
        reg_bool_rx(engine, ctx, "ctcss",
            |app, i| app.rx(i).map(|r| r.ctcss_on).unwrap_or(false),
            |app, i, b| app.set_rx_ctcss(i, b),
        );
        reg_f32_rx(engine, ctx, "ctcss_hz",
            |app, i| app.rx(i).map(|r| r.ctcss_hz).unwrap_or(0.0),
            |app, i, v| app.set_rx_ctcss_freq(i, v),
        );

        // --- SAM sub-mode (0=DSB 1=LSB 2=USB) ---
        reg_i32_rx(engine, ctx, "sam_submode",
            |app, i| app.rx(i).map(|r| r.sam_submode as i32).unwrap_or(0),
            |app, i, v| app.set_rx_sam_submode(i, v.clamp(0, 2) as u8),
        );

        // --- agc (string) ---
        let c = ctx.clone();
        engine.register_get("agc", move |rx: &mut Rx| -> String {
            c.with_app(|app| {
                app.rx(rx.index as usize).map(|r| agc_to_str(r.agc_mode).to_string()).unwrap_or_default()
            })
            .unwrap_or_default()
        });
        let c = ctx.clone();
        engine.register_set("agc", move |rx: &mut Rx, s: String| -> Result<(), Box<rhai::EvalAltResult>> {
            let agc = parse_agc(&s).map_err(rhai_err)?;
            c.with_app(|app| app.set_rx_agc(rx.index, agc)).map_err(rhai_err)?;
            Ok(())
        });

        // --- nb / nb2 / anf / bin / tnf (toggle flags) ---
        for flag in ["nb", "nb2", "anf", "bin", "tnf"] {
            let c = ctx.clone();
            let f = flag.to_string();
            engine.register_get(flag, move |rx: &mut Rx| -> bool {
                let rxi = rx.index as usize;
                let fs = f.as_str();
                c.with_app(|app| {
                    app.rx(rxi).map(|r| match fs {
                        "nb"  => r.nb,
                        "nb2" => r.nb2,
                        "anf" => r.anf,
                        "bin" => r.bin,
                        "tnf" => r.tnf,
                        _ => false,
                    }).unwrap_or(false)
                }).unwrap_or(false)
            });
            let c = ctx.clone();
            let f = flag.to_string();
            engine.register_set(flag, move |rx: &mut Rx, target: bool| {
                let rxi = rx.index;
                let fs = f.clone();
                let _ = c.with_app(|app| {
                    let cur = app.rx(rxi as usize).map(|r| match fs.as_str() {
                        "nb"  => r.nb,
                        "nb2" => r.nb2,
                        "anf" => r.anf,
                        "bin" => r.bin,
                        "tnf" => r.tnf,
                        _ => false,
                    }).unwrap_or(false);
                    if cur != target {
                        app.toggle_rx_flag(rxi, fs.as_str());
                    }
                });
            });
        }

        // --- rit (i32 Hz, display-only until TX path lands) ---
        let c = ctx.clone();
        engine.register_get("rit", move |rx: &mut Rx| -> i64 {
            c.with_app(|app| app.rx(rx.index as usize).map(|r| r.rit_hz as i64).unwrap_or(0))
                .unwrap_or(0)
        });
        let c = ctx.clone();
        engine.register_set("rit", move |rx: &mut Rx, hz: i64| {
            let _ = c.with_app(|app| app.set_rx_rit(rx.index, hz as i32));
        });

        // --- eq_enabled ---
        reg_bool_rx(engine, ctx, "eq_enabled",
            |app, i| app.rx(i).map(|r| r.eq_enabled).unwrap_or(false),
            |app, i, b| app.set_rx_eq_enabled(i, b),
        );

        // --- eq_gains (Array of 11 ints) ---
        let c = ctx.clone();
        engine.register_get("eq_gains", move |rx: &mut Rx| -> Array {
            c.with_app(|app| {
                app.rx(rx.index as usize)
                    .map(|r| r.eq_gains.iter().map(|g| Dynamic::from(*g as i64)).collect::<Array>())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
        });
        let c = ctx.clone();
        engine.register_set("eq_gains", move |rx: &mut Rx, arr: Array| -> Result<(), Box<rhai::EvalAltResult>> {
            if arr.len() != 11 {
                return Err(rhai_err(format!(
                    "eq_gains requires array of 11 ints, got {}", arr.len()
                )));
            }
            let mut gains = [0i32; 11];
            for (i, v) in arr.iter().enumerate() {
                gains[i] = v.as_int().map_err(|t| rhai_err(format!("eq_gains[{i}] not int, got {t}")))? as i32;
            }
            c.with_app(|app| app.set_rx_eq_gains(rx.index, gains)).map_err(rhai_err)?;
            Ok(())
        });

        // --- s_meter (RO) ---
        let c = ctx.clone();
        engine.register_get("s_meter", move |rx: &mut Rx| -> f64 {
            let idx = rx.index as usize;
            c.with_app(|app| {
                app.telemetry_snapshot()
                    .map(|t| t.rx[idx.min(t.rx.len() - 1)].s_meter_db as f64)
                    .unwrap_or(-140.0)
            })
            .unwrap_or(-140.0)
        });

        // --- spectrum (RO) ---
        let c = ctx.clone();
        engine.register_get("spectrum", move |rx: &mut Rx| -> Array {
            let idx = rx.index as usize;
            c.with_app(|app| {
                app.telemetry_snapshot()
                    .map(|t| {
                        t.rx[idx.min(t.rx.len() - 1)]
                            .spectrum_bins_db
                            .iter()
                            .map(|v| Dynamic::from(*v as f64))
                            .collect::<Array>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default()
        });

        // --- center_freq (RO) ---
        let c = ctx.clone();
        engine.register_get("center_freq", move |rx: &mut Rx| -> i64 {
            let idx = rx.index as usize;
            c.with_app(|app| {
                app.telemetry_snapshot()
                    .map(|t| t.rx[idx.min(t.rx.len() - 1)].center_freq_hz as i64)
                    .unwrap_or(0)
            })
            .unwrap_or(0)
        });

        // --- Display / ToString ---
        engine.register_fn("to_string", |rx: &mut Rx| format!("Rx({})", rx.index));
    }
}

fn reg_bool_rx<G, S>(
    engine: &mut Engine,
    ctx: &ApiCtx,
    name: &'static str,
    getter: G,
    setter: S,
) where
    G: Fn(&arion_app::App, usize) -> bool + Clone + 'static,
    S: Fn(&mut arion_app::App, u8, bool) + Clone + 'static,
{
    let c = ctx.clone();
    let g = getter.clone();
    engine.register_get(name, move |rx: &mut Rx| -> bool {
        c.with_app(|app| g(app, rx.index as usize)).unwrap_or(false)
    });
    let c = ctx.clone();
    let s = setter;
    engine.register_set(name, move |rx: &mut Rx, b: bool| {
        let _ = c.with_app(|app| s(app, rx.index, b));
    });
}

fn reg_f32_rx<G, S>(
    engine: &mut Engine,
    ctx: &ApiCtx,
    name: &'static str,
    getter: G,
    setter: S,
) where
    G: Fn(&arion_app::App, usize) -> f32 + Clone + 'static,
    S: Fn(&mut arion_app::App, u8, f32) + Clone + 'static,
{
    let c = ctx.clone();
    let g = getter.clone();
    engine.register_get(name, move |rx: &mut Rx| -> f64 {
        c.with_app(|app| g(app, rx.index as usize) as f64).unwrap_or(0.0)
    });
    let c = ctx.clone();
    let s = setter;
    engine.register_set(name, move |rx: &mut Rx, v: f64| {
        let _ = c.with_app(|app| s(app, rx.index, v as f32));
    });
}

fn reg_i32_rx<G, S>(
    engine: &mut Engine,
    ctx: &ApiCtx,
    name: &'static str,
    getter: G,
    setter: S,
) where
    G: Fn(&arion_app::App, usize) -> i32 + Clone + 'static,
    S: Fn(&mut arion_app::App, u8, i32) + Clone + 'static,
{
    let c = ctx.clone();
    let g = getter.clone();
    engine.register_get(name, move |rx: &mut Rx| -> i64 {
        c.with_app(|app| g(app, rx.index as usize) as i64).unwrap_or(0)
    });
    let c = ctx.clone();
    let s = setter;
    engine.register_set(name, move |rx: &mut Rx, v: i64| {
        let _ = c.with_app(|app| s(app, rx.index, v as i32));
    });
}
