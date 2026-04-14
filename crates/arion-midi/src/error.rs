use thiserror::Error;

#[derive(Debug, Error)]
pub enum MidiError {
    #[error("MIDI init failed: {0}")]
    Init(#[from] midir::InitError),

    #[error("MIDI connect failed: {0}")]
    Connect(String),

    #[error("MIDI port not found: {0}")]
    DeviceNotFound(String),
}

impl From<midir::ConnectError<midir::MidiInput>> for MidiError {
    fn from(e: midir::ConnectError<midir::MidiInput>) -> Self {
        MidiError::Connect(e.to_string())
    }
}
