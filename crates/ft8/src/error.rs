use thiserror::Error;

#[derive(Debug, Error)]
pub enum Ft8Error {
    #[error("invalid message text (contains NUL)")]
    InvalidText,
    #[error("ftx_message_encode failed: rc={0}")]
    EncodeFailed(i32),
    #[error("FT8 internal error")]
    Internal,
}
