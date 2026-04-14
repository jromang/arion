use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use arc_swap::ArcSwap;
use arion_app::protocol::{Action, StateSnapshot};
use arion_core::Telemetry;

/// Everything a request handler needs to serve the REST API.
///
/// Reads go through the ArcSwap snapshots (lock-free). Writes go
/// through `action_tx` which the UI thread drains each frame and
/// applies to `App`. MIDI binding edits bypass `Action` and mutate
/// `midi_mapping` directly because the listener reads it via ArcSwap.
#[derive(Clone)]
pub struct ApiContext {
    pub snapshot:       Arc<ArcSwap<StateSnapshot>>,
    pub telemetry:      Arc<ArcSwap<Telemetry>>,
    pub action_tx:      mpsc::Sender<Action>,
    pub script_tx:      Option<mpsc::Sender<ScriptRequest>>,
    pub midi_mapping:   Option<arion_midi::SharedMapping>,
    pub midi_last_event: Arc<ArcSwap<Option<arion_midi::MidiEvent>>>,
    pub midi_persist:   bool,
    pub started_at:     Instant,
    pub build_version:  &'static str,
    /// Optional path where MIDI mapping is persisted on change.
    /// `None` disables persistence.
    pub midi_persist_path: Option<PathBuf>,
}

/// A Rhai script submitted via `POST /scripts/eval`. The UI thread
/// picks it up, runs it in its `ScriptEngine`, and sends the result
/// back through `reply`.
pub struct ScriptRequest {
    pub source: String,
    pub reply:  mpsc::SyncSender<ScriptReply>,
}

/// Result of a script evaluation.
pub struct ScriptReply {
    pub output: String,
    pub error:  Option<String>,
}
