//! Rhai scripting engine for Arion.
//!
//! Public façade — see [`ScriptEngine`]. The engine is designed to be
//! called from the UI thread (egui or TUI) once per user action via
//! [`ScriptEngine::run_line`]. Long-running scripts are capped by
//! `Engine::set_max_operations` so the UI stays responsive.
//!
//! Architectural principles:
//! - **Single source of truth**: the engine never caches `App` state.
//!   Every getter reads live.
//! - **Scoped borrow**: a raw `*mut App` pointer is written to an
//!   `Rc<RefCell<Option<*mut App>>>` at the start of `run_line` /
//!   `invoke_callback` and cleared before the call returns, so closures
//!   registered in the engine only see a valid App during a call.
//! - **Registry**: each family of bindings lives in a `modules/*.rs`
//!   file and implements `ScriptModule`. Adding an API surface is a
//!   one-file change + one line in `modules::register_builtins`.

pub mod ctx;
pub mod engine;
pub mod error;
pub mod help_data;
pub mod modules;
pub mod ui_tree;

pub use engine::{ReplLine, ReplLineKind, ScriptEngine};
pub use error::ScriptError;
pub use ui_tree::{FnHandle, ScriptWindow, UiState, Widget};

use std::path::PathBuf;

/// Default path of the user's startup script:
/// `~/.config/arion/startup.rhai` on Linux, the platform equivalent
/// elsewhere (macOS `Application Support`, Windows `%APPDATA%`).
/// Returns `None` only on headless systems with no `HOME`.
pub fn startup_script_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("rs", "arion", "arion")
        .map(|p| p.config_dir().join("startup.rhai"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arion_app::{App, AppOptions};

    fn new_app() -> App {
        App::new(AppOptions::default())
    }

    #[test]
    fn run_line_sets_rx_frequency() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("radio[0].freq = 14074000", &mut app);
        assert_eq!(app.rx(0).unwrap().frequency_hz, 14_074_000);
    }

    #[test]
    fn free_functions_work() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("freq(radio.rx(0), 7074000)", &mut app);
        assert_eq!(app.rx(0).unwrap().frequency_hz, 7_074_000);
        eng.run_line("mode(radio.rx(0), \"LSB\")", &mut app);
        assert_eq!(app.rx(0).unwrap().mode, arion_core::WdspMode::Lsb);
        eng.run_line("volume(radio.rx(0), 0.75)", &mut app);
        assert!((app.rx(0).unwrap().volume - 0.75).abs() < 1e-5);
    }

    #[test]
    fn property_setters_and_getters() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("radio[0].mode = \"USB\"", &mut app);
        assert_eq!(app.rx(0).unwrap().mode, arion_core::WdspMode::Usb);
        eng.run_line("radio[0].agc = \"Fast\"", &mut app);
        assert_eq!(app.rx(0).unwrap().agc_mode, arion_app::AgcPreset::Fast);
        eng.run_line("radio[0].muted = true", &mut app);
        assert!(app.rx(0).unwrap().muted);
        eng.run_line("radio[0].volume = 0.4", &mut app);
        assert!((app.rx(0).unwrap().volume - 0.4).abs() < 1e-5);
    }

    #[test]
    fn eq_gains_round_trip() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line(
            "radio[0].eq_gains = [1,2,3,4,5,6,7,8,9,10,11]",
            &mut app,
        );
        assert_eq!(app.rx(0).unwrap().eq_gains, [1,2,3,4,5,6,7,8,9,10,11]);
    }

    #[test]
    fn band_jump() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("band(\"40\")", &mut app);
        let rx = app.active_rx();
        // 40m default anchor is 7.074 MHz (see band_stack_default_seeded).
        assert_eq!(app.rx(rx).unwrap().frequency_hz, 7_074_000);
    }

    #[test]
    fn filter_preset_sets_passband() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("radio[0].mode = \"USB\"", &mut app);
        eng.run_line("filter_preset(radio.rx(0), \"2.4K\")", &mut app);
        let r = app.rx(0).unwrap();
        assert!((r.filter_hi - r.filter_lo - 2400.0).abs() < 0.5);
    }

    #[test]
    fn window_toggle() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("window(\"memories\", true)", &mut app);
        assert!(app.window_open(arion_app::WindowKind::Memories));
        eng.run_line("toggle_window(\"memories\")", &mut app);
        assert!(!app.window_open(arion_app::WindowKind::Memories));
    }

    #[test]
    fn memory_save_and_load() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("freq(radio.rx(0), 10123000)", &mut app);
        eng.run_line("memory_save(\"test\")", &mut app);
        assert_eq!(app.memories().len(), 1);
        assert_eq!(app.memories()[0].freq_hz, 10_123_000);
        eng.run_line("freq(radio.rx(0), 3500000)", &mut app);
        eng.run_line("memory_load(0)", &mut app);
        assert_eq!(app.rx(app.active_rx()).unwrap().frequency_hz, 10_123_000);
    }

    #[test]
    fn radio_properties_read_live() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        app.set_radio_ip("192.168.1.99".to_string());
        eng.run_line("radio.ip", &mut app);
        // If we got here without error, the getter worked.
        let out = eng.output();
        let last_result = out.iter().rev().find(|l| l.kind == ReplLineKind::Result).unwrap();
        assert!(last_result.text.contains("192.168.1.99"));
    }

    #[test]
    fn active_rx_num_rx() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("active_rx(1)", &mut app);
        assert_eq!(app.active_rx(), 1);
    }

    #[test]
    fn nr3_flag_persists() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("radio[0].nr3 = true", &mut app);
        assert!(app.rx(0).unwrap().nr3);
        eng.run_line("radio[0].nr3 = false", &mut app);
        assert!(!app.rx(0).unwrap().nr3);
    }

    #[test]
    fn flag_toggle_via_free_fn() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("anf(radio.rx(0), true)", &mut app);
        assert!(app.rx(0).unwrap().anf);
    }

    #[test]
    fn ui_window_builder_collects_children() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line(
            r#"window("a", "A", || { button("x", || {}); label("hi"); })"#,
            &mut app,
        );
        let ui = eng.ui_state();
        let ui = ui.borrow();
        let w = ui.windows.get("a").expect("window a exists");
        assert_eq!(w.title, "A");
        match &w.root {
            Widget::VBox(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(children[0], Widget::Button { .. }));
                assert!(matches!(children[1], Widget::Label(_)));
            }
            _ => panic!("root should be VBox"),
        }
    }

    #[test]
    fn ui_slider_and_on_change_registered() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line(
            r#"window("w", "W", || { slider("V", "vol", 0.0, 1.0); }); on_change("vol", |v| { print(v); })"#,
            &mut app,
        );
        let ui = eng.ui_state();
        let ui = ui.borrow();
        assert!(ui.values.contains_key("vol"));
        assert!(ui.on_change.contains_key("vol"));
    }

    #[test]
    fn ui_window_show_hide_toggle() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line(r#"window("x", "X", || { label("z"); })"#, &mut app);
        assert!(eng.ui_state().borrow().windows["x"].open);
        eng.run_line(r#"window_hide("x")"#, &mut app);
        assert!(!eng.ui_state().borrow().windows["x"].open);
        eng.run_line(r#"window_toggle("x")"#, &mut app);
        assert!(eng.ui_state().borrow().windows["x"].open);
        eng.run_line(r#"window_show("x")"#, &mut app);
        assert!(eng.ui_state().borrow().windows["x"].open);
    }

    #[test]
    fn ui_menu_item_registered() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line(r#"menu_item("Tools/Foo", || {})"#, &mut app);
        assert_eq!(eng.ui_state().borrow().menu_items.len(), 1);
        assert_eq!(eng.ui_state().borrow().menu_items[0].0, "Tools/Foo");
    }

    #[test]
    fn run_script_multiline_mutates_state() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        let src = r#"
            radio[0].freq = 7074000;
            radio[0].mode = "LSB";
            radio[0].volume = 0.25;
            radio[0].nr3 = true;
        "#;
        eng.run_script(src, &mut app).expect("script must succeed");
        let r = app.rx(0).unwrap();
        assert_eq!(r.frequency_hz, 7_074_000);
        assert_eq!(r.mode, arion_core::WdspMode::Lsb);
        assert!((r.volume - 0.25).abs() < 1e-5);
        assert!(r.nr3);
    }

    #[test]
    fn run_script_error_surfaces_as_repl_line() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        let res = eng.run_script("let x = bogus_symbol_zzz + 1;", &mut app);
        assert!(res.is_err());
        assert!(eng.output().iter().any(|l| l.kind == ReplLineKind::Error));
    }

    #[test]
    fn startup_script_path_has_expected_tail() {
        if let Some(p) = startup_script_path() {
            assert!(p.ends_with("startup.rhai"));
        }
    }

    #[test]
    fn help_overview_returns_non_empty() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("help()", &mut app);
        let out = eng.output();
        let last = out.iter().rev().find(|l| l.kind == ReplLineKind::Result).unwrap();
        assert!(last.text.contains("Topics"));
    }

    #[test]
    fn help_topic_returns_entry() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("help(\"radio\")", &mut app);
        let last = eng.output().iter().rev()
            .find(|l| l.kind == ReplLineKind::Result).unwrap();
        assert!(last.text.contains("radio"));
    }

    #[test]
    fn help_unknown_topic_returns_hint() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("help(\"xyzxyz\")", &mut app);
        let last = eng.output().iter().rev()
            .find(|l| l.kind == ReplLineKind::Result).unwrap();
        assert!(last.text.contains("no topic"));
    }

    #[test]
    fn help_coherence_core_functions_all_documented() {
        let topics = help_data::help_topics();
        let required = [
            "overview", "radio", "rx", "actions", "modes", "bands",
            "filters", "agc", "memories", "ui", "startup",
            "freq", "mode", "volume", "mute", "lock",
            "filter", "filter_preset",
            "nr3", "nr4", "nb", "nb2", "anf", "bin", "tnf",
            "eq", "eq_band", "band", "tune",
            "active_rx", "num_rx", "connect", "disconnect",
            "memory_save", "memory_load", "memory_delete",
            "save", "audio_device",
            "window", "button", "slider", "checkbox",
            "on_change", "menu_item", "help",
        ];
        for k in required {
            assert!(topics.contains_key(k), "help topic missing: {k}");
        }
    }

    #[test]
    fn unknown_mode_errors() {
        let mut app = new_app();
        let mut eng = ScriptEngine::new();
        eng.run_line("mode(radio.rx(0), \"BOGUS\")", &mut app);
        let out = eng.output();
        assert!(out.iter().any(|l| l.kind == ReplLineKind::Error));
    }
}
