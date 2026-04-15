use thiserror::Error;

#[derive(Debug, Error)]
pub enum LiquidError {
    #[error("liquid-dsp error: {0}")]
    Native(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
}
