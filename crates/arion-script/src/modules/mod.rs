//! Script modules: each registers a slice of the Rhai API.
//!
//! Adding a new family of bindings is a single-file change: create
//! `modules/foo.rs`, implement [`ScriptModule`] on a zero-sized
//! struct, then list it in [`register_builtins`].

pub mod help;
pub mod memory;
pub mod radio;
pub mod rx;
pub mod ui;
pub mod window;

use rhai::Engine;

use crate::ctx::ApiCtx;

pub trait ScriptModule {
    fn register(&self, engine: &mut Engine, ctx: &ApiCtx);
}

pub fn register_builtins(engine: &mut Engine, ctx: &ApiCtx) {
    radio::RadioModule.register(engine, ctx);
    rx::RxModule.register(engine, ctx);
    memory::MemoryModule.register(engine, ctx);
    window::WindowModule.register(engine, ctx);
    ui::UiModule.register(engine, ctx);
    help::HelpModule.register(engine, ctx);
}

// --- Shared string → enum parsers --------------------------------------

use arion_app::{AgcPreset, Band, FilterPreset, WindowKind};
use arion_core::WdspMode;

use crate::error::ScriptError;

pub fn parse_mode(s: &str) -> Result<WdspMode, ScriptError> {
    let up = s.to_ascii_uppercase();
    match up.as_str() {
        "LSB"            => Ok(WdspMode::Lsb),
        "USB"            => Ok(WdspMode::Usb),
        "DSB"            => Ok(WdspMode::Dsb),
        "CWL"            => Ok(WdspMode::CwL),
        "CWU" | "CW"     => Ok(WdspMode::CwU),
        "FM"             => Ok(WdspMode::Fm),
        "AM"             => Ok(WdspMode::Am),
        "DIGU" | "DIGITAL U" => Ok(WdspMode::DigU),
        "DIGL" | "DIGITAL L" => Ok(WdspMode::DigL),
        "SAM"            => Ok(WdspMode::Sam),
        "SPEC"           => Ok(WdspMode::Spec),
        "DRM"            => Ok(WdspMode::Drm),
        _ => Err(ScriptError::UnknownMode(s.to_string())),
    }
}

pub fn mode_to_str(m: WdspMode) -> &'static str {
    match m {
        WdspMode::Lsb  => "LSB",
        WdspMode::Usb  => "USB",
        WdspMode::Dsb  => "DSB",
        WdspMode::CwL  => "CWL",
        WdspMode::CwU  => "CWU",
        WdspMode::Fm   => "FM",
        WdspMode::Am   => "AM",
        WdspMode::DigU => "DIGU",
        WdspMode::DigL => "DIGL",
        WdspMode::Sam  => "SAM",
        WdspMode::Spec => "SPEC",
        WdspMode::Drm  => "DRM",
    }
}

pub fn parse_agc(s: &str) -> Result<AgcPreset, ScriptError> {
    match s.to_ascii_lowercase().as_str() {
        "off"  => Ok(AgcPreset::Off),
        "long" => Ok(AgcPreset::Long),
        "slow" => Ok(AgcPreset::Slow),
        "med"  | "medium" => Ok(AgcPreset::Med),
        "fast" => Ok(AgcPreset::Fast),
        _ => Err(ScriptError::UnknownAgc(s.to_string())),
    }
}

pub fn agc_to_str(a: AgcPreset) -> &'static str {
    match a {
        AgcPreset::Off  => "Off",
        AgcPreset::Long => "Long",
        AgcPreset::Slow => "Slow",
        AgcPreset::Med  => "Med",
        AgcPreset::Fast => "Fast",
    }
}

pub fn parse_band(s: &str) -> Result<Band, ScriptError> {
    match s {
        "160" => Ok(Band::M160),
        "80"  => Ok(Band::M80),
        "60"  => Ok(Band::M60),
        "40"  => Ok(Band::M40),
        "30"  => Ok(Band::M30),
        "20"  => Ok(Band::M20),
        "17"  => Ok(Band::M17),
        "15"  => Ok(Band::M15),
        "12"  => Ok(Band::M12),
        "10"  => Ok(Band::M10),
        "6"   => Ok(Band::M6),
        _ => Err(ScriptError::UnknownBand(s.to_string())),
    }
}

pub fn parse_filter_preset(s: &str) -> Result<FilterPreset, ScriptError> {
    match s {
        "6.0K" | "6K" => Ok(FilterPreset::F6000),
        "4.0K" | "4K" => Ok(FilterPreset::F4000),
        "2.7K"        => Ok(FilterPreset::F2700),
        "2.4K"        => Ok(FilterPreset::F2400),
        "1.8K"        => Ok(FilterPreset::F1800),
        "1.0K" | "1K" => Ok(FilterPreset::F1000),
        "600"         => Ok(FilterPreset::F600),
        "400"         => Ok(FilterPreset::F400),
        "250"         => Ok(FilterPreset::F250),
        "100"         => Ok(FilterPreset::F100),
        _ => Err(ScriptError::UnknownFilterPreset(s.to_string())),
    }
}

pub fn parse_window(s: &str) -> Result<WindowKind, ScriptError> {
    match s.to_ascii_lowercase().as_str() {
        "memories"    => Ok(WindowKind::Memories),
        "bandstack"   => Ok(WindowKind::BandStack),
        "multimeter"  => Ok(WindowKind::Multimeter),
        "setup"       => Ok(WindowKind::Setup),
        "repl"        => Ok(WindowKind::Repl),
        "eq"          => Ok(WindowKind::Eq),
        _ => Err(ScriptError::UnknownWindow(s.to_string())),
    }
}

pub fn rhai_err<T: std::fmt::Display>(e: T) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(
        rhai::Dynamic::from(e.to_string()),
        rhai::Position::NONE,
    ))
}
