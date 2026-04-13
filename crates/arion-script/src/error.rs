//! Error types for the scripting engine.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScriptError {
    #[error("rhai error: {0}")]
    Rhai(String),

    #[error("app is not bound (call outside of run_line/invoke_callback)")]
    AppUnbound,

    #[error("no such rx: {0}")]
    NoSuchRx(usize),

    #[error("unknown mode: {0}")]
    UnknownMode(String),

    #[error("unknown agc preset: {0}")]
    UnknownAgc(String),

    #[error("unknown band: {0}")]
    UnknownBand(String),

    #[error("unknown filter preset: {0}")]
    UnknownFilterPreset(String),

    #[error("unknown window kind: {0}")]
    UnknownWindow(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl From<Box<rhai::EvalAltResult>> for ScriptError {
    fn from(e: Box<rhai::EvalAltResult>) -> Self {
        ScriptError::Rhai(e.to_string())
    }
}
