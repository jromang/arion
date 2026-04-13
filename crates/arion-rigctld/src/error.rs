//! Error types for the rigctld server.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum RigctldError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("server not running")]
    NotRunning,
}
