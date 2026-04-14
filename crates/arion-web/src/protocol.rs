//! Wire format for the control WebSocket.
//!
//! Server → client: [`Envelope::State`] snapshot (pushed on a timer).
//! Client → server: [`ClientEnvelope::Action`] — apply to the `App`.
//!
//! The domain DTOs (`StateSnapshot`, `Action`, mode conversion
//! helpers) live in `arion_app::protocol` so any transport crate
//! (REST API, TCI) can project/dispatch the same types.

use serde::{Deserialize, Serialize};

pub use arion_app::protocol::{Action, RxSnapshot, StateSnapshot};

#[derive(Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Envelope<'a> {
    State(StateSnapshot),
    Webrtc(WebrtcServer<'a>),
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientEnvelope {
    Action(Action),
    Webrtc(WebrtcClient),
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebrtcClient {
    Offer { sdp: String },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebrtcServer<'a> {
    Answer { sdp: &'a str },
    Error  { message: &'a str },
}
