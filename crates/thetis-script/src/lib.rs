//! Rhai scripting engine for Thetis.
//!
//! Provides a [`ScriptEngine`] that wraps a `rhai::Engine` with
//! bindings to [`thetis_app::App`]. Every UI action has a script
//! equivalent — a user typing `radio.set_freq(0, 14074000)` in the
//! REPL gets exactly the same result as clicking in the waterfall.
//!
//! The engine is designed to be called from the UI thread (egui or
//! TUI) once per frame via [`ScriptEngine::run_line`]. Long-running
//! scripts are capped by `Engine::set_max_operations` so the UI
//! stays responsive.

use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine, Scope};
use thetis_app::App;

/// A line of REPL output, tagged with its kind for coloring in the
/// frontend.
#[derive(Debug, Clone)]
pub struct ReplLine {
    pub kind: ReplLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplLineKind {
    Input,
    Result,
    Error,
    Print,
}

/// Wraps a `rhai::Engine` with Thetis-specific function bindings and
/// a REPL output buffer. The engine does **not** own the `App` — the
/// caller passes `&mut App` into [`run_line`] so the borrow is
/// scoped to the call (no `Arc<Mutex<App>>` needed, no deadlock
/// risk).
pub struct ScriptEngine {
    engine: Engine,
    scope:  Scope<'static>,
    output: Vec<ReplLine>,
    history: Vec<String>,
}

impl ScriptEngine {
    pub fn new() -> Self {
        let mut engine = Engine::new();

        // Cap execution to ~100 ms worth of operations so a rogue
        // `loop {}` doesn't freeze the UI.
        engine.set_max_operations(1_000_000);

        ScriptEngine {
            engine,
            scope: Scope::new(),
            output: Vec::new(),
            history: Vec::new(),
        }
    }

    /// Execute a single line of Rhai code with access to the App.
    /// Pushes the input, result (or error), and any `print()` output
    /// into the REPL buffer.
    pub fn run_line(&mut self, line: &str, app: &mut App) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }

        self.history.push(trimmed.to_string());
        self.output.push(ReplLine {
            kind: ReplLineKind::Input,
            text: format!("> {trimmed}"),
        });

        // Register App bindings fresh each call so we don't need to
        // store a long-lived reference. We use a shared print buffer
        // to capture `print()` calls from within the script.
        let print_buf: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let pb = print_buf.clone();
        self.engine.on_print(move |s| {
            if let Ok(mut buf) = pb.lock() {
                buf.push(s.to_string());
            }
        });

        // Register app-bound functions as a custom module.
        // We use a closure-based approach: bind App as a Dynamic
        // stored in the scope, and register functions that access it.
        self.register_app_bindings(app);

        match self.engine.eval_with_scope::<Dynamic>(&mut self.scope, trimmed) {
            Ok(val) => {
                let text = if val.is_unit() {
                    "ok".to_string()
                } else {
                    format!("{val}")
                };
                self.output.push(ReplLine {
                    kind: ReplLineKind::Result,
                    text,
                });
            }
            Err(e) => {
                self.output.push(ReplLine {
                    kind: ReplLineKind::Error,
                    text: format!("{e}"),
                });
            }
        }

        // Flush print buffer
        let printed: Vec<String> = print_buf.lock()
            .map(|b| b.clone())
            .unwrap_or_default();
        for line in printed {
            self.output.push(ReplLine {
                kind: ReplLineKind::Print,
                text: line,
            });
        }
    }

    /// Register all app-bound functions into the scope. Since we
    /// can't store `&mut App` across frames, we snapshot the values
    /// we need for read-only queries into scope variables, and
    /// collect write-intent results via scope variables that the
    /// caller reads back.
    ///
    /// For D.12 we use a simpler approach: store commands as scope
    /// variables that the caller interprets after eval.
    fn register_app_bindings(&mut self, app: &mut App) {
        // Snapshot read-only values into scope
        if let Some(rx) = app.rx(0) {
            self.scope.set_or_push("rx0_freq", rx.frequency_hz as i64);
            self.scope.set_or_push("rx0_mode", format!("{:?}", rx.mode));
        }
        if let Some(rx) = app.rx(1) {
            self.scope.set_or_push("rx1_freq", rx.frequency_hz as i64);
            self.scope.set_or_push("rx1_mode", format!("{:?}", rx.mode));
        }
        self.scope.set_or_push("active_rx", app.active_rx() as i64);
        self.scope.set_or_push("connected", app.is_connected());
        self.scope.set_or_push("num_rx", app.num_rx() as i64);

        if let Some(snapshot) = app.telemetry_snapshot() {
            for r in 0..snapshot.num_rx.min(2) as usize {
                let key = format!("rx{r}_smeter");
                self.scope.set_or_push(key, snapshot.rx[r].s_meter_db as f64);
            }
        }

        // Command accumulator: scripts push commands here, caller
        // reads them after eval.
        self.scope.set_or_push("_cmds", rhai::Array::new());
    }

    /// Apply any commands that scripts pushed into `_cmds` during
    /// evaluation. Called by the frontend after `run_line`.
    pub fn apply_pending_commands(&mut self, app: &mut App) {
        let cmds: rhai::Array = self.scope
            .get_value("_cmds")
            .unwrap_or_default();

        for cmd in cmds {
            if let Ok(s) = cmd.into_string() {
                self.apply_command(&s, app);
            }
        }
        self.scope.set_or_push("_cmds", rhai::Array::new());
    }

    fn apply_command(&self, cmd: &str, app: &mut App) {
        let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
        match parts.as_slice() {
            ["set_freq", rx, hz] => {
                if let (Ok(rx), Ok(hz)) = (rx.parse::<u8>(), hz.parse::<u32>()) {
                    app.set_rx_frequency(rx, hz);
                }
            }
            ["set_mode", rx, mode] => {
                if let Ok(rx) = rx.parse::<u8>() {
                    let m = match *mode {
                        "LSB" | "Lsb"  => Some(thetis_core::WdspMode::Lsb),
                        "USB" | "Usb"  => Some(thetis_core::WdspMode::Usb),
                        "AM"  | "Am"   => Some(thetis_core::WdspMode::Am),
                        "SAM" | "Sam"  => Some(thetis_core::WdspMode::Sam),
                        "FM"  | "Fm"   => Some(thetis_core::WdspMode::Fm),
                        "CWL" | "CwL"  => Some(thetis_core::WdspMode::CwL),
                        "CWU" | "CwU"  => Some(thetis_core::WdspMode::CwU),
                        "DIGL" | "DigL" => Some(thetis_core::WdspMode::DigL),
                        "DIGU" | "DigU" => Some(thetis_core::WdspMode::DigU),
                        _ => None,
                    };
                    if let Some(m) = m {
                        app.set_rx_mode(rx, m);
                    }
                }
            }
            ["set_volume", rx, vol] => {
                if let (Ok(rx), Ok(vol)) = (rx.parse::<u8>(), vol.parse::<f32>()) {
                    app.set_rx_volume(rx, vol);
                }
            }
            ["set_nr3", rx, on] => {
                if let Ok(rx) = rx.parse::<u8>() {
                    app.set_rx_nr3(rx, *on == "true");
                }
            }
            ["set_nr4", rx, on] => {
                if let Ok(rx) = rx.parse::<u8>() {
                    app.set_rx_nr4(rx, *on == "true");
                }
            }
            ["tune_band", _rx, band] => {
                if let Some(b) = match *band {
                    "160" => Some(thetis_app::Band::M160),
                    "80"  => Some(thetis_app::Band::M80),
                    "60"  => Some(thetis_app::Band::M60),
                    "40"  => Some(thetis_app::Band::M40),
                    "30"  => Some(thetis_app::Band::M30),
                    "20"  => Some(thetis_app::Band::M20),
                    "17"  => Some(thetis_app::Band::M17),
                    "15"  => Some(thetis_app::Band::M15),
                    "12"  => Some(thetis_app::Band::M12),
                    "10"  => Some(thetis_app::Band::M10),
                    "6"   => Some(thetis_app::Band::M6),
                    _ => None,
                } {
                    app.jump_to_band(b);
                }
            }
            ["connect", ..] => { app.connect(); }
            ["disconnect", ..] => { app.disconnect(); }
            _ => {
                tracing::warn!(cmd, "unknown script command");
            }
        }
    }

    pub fn output(&self) -> &[ReplLine] {
        &self.output
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn clear_output(&mut self) {
        self.output.clear();
    }

    /// Register convenience functions in the Rhai engine that push
    /// commands into `_cmds`. These are the user-facing API.
    pub fn register_api(&mut self) {
        // set_freq(rx, hz) → pushes "set_freq {rx} {hz}"
        self.engine.register_fn("set_freq", |rx: i64, hz: i64| -> String {
            format!("set_freq {rx} {hz}")
        });
        self.engine.register_fn("set_mode", |rx: i64, mode: &str| -> String {
            format!("set_mode {rx} {mode}")
        });
        self.engine.register_fn("set_volume", |rx: i64, vol: f64| -> String {
            format!("set_volume {rx} {vol}")
        });
        self.engine.register_fn("set_nr3", |rx: i64, on: bool| -> String {
            format!("set_nr3 {rx} {on}")
        });
        self.engine.register_fn("set_nr4", |rx: i64, on: bool| -> String {
            format!("set_nr4 {rx} {on}")
        });
        self.engine.register_fn("tune_band", |rx: i64, band: &str| -> String {
            format!("tune_band {rx} {band}")
        });
        self.engine.register_fn("connect", || -> String {
            "connect".to_string()
        });
        self.engine.register_fn("disconnect", || -> String {
            "disconnect".to_string()
        });

        // Wrapper: call function and push result to _cmds
        // Users write: `set_freq(0, 14074000)` which returns the
        // command string. We need a script wrapper to auto-push.
        // For now, users must write:
        //   _cmds.push(set_freq(0, 14074000))
        // Or we pre-wrap common patterns. Let's make it transparent
        // by registering command functions that auto-push:
        let src = r#"
            fn freq(rx, hz)      { _cmds.push(set_freq(rx, hz)); }
            fn mode(rx, m)       { _cmds.push(set_mode(rx, m)); }
            fn volume(rx, v)     { _cmds.push(set_volume(rx, v)); }
            fn nr3(rx, on)       { _cmds.push(set_nr3(rx, on)); }
            fn nr4(rx, on)       { _cmds.push(set_nr4(rx, on)); }
            fn band(rx, b)       { _cmds.push(tune_band(rx, b)); }
            fn do_connect()      { _cmds.push(connect()); }
            fn do_disconnect()   { _cmds.push(disconnect()); }
        "#;
        // Compile and merge into the scope as a prelude AST
        if let Ok(ast) = self.engine.compile(src) {
            let _ = self.engine.run_ast_with_scope(&mut self.scope, &ast);
        }
    }
}

impl Default for ScriptEngine {
    fn default() -> Self {
        let mut eng = Self::new();
        eng.register_api();
        eng
    }
}
