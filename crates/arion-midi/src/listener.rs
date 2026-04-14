use std::sync::{mpsc, Arc};

use arc_swap::ArcSwap;
use midir::{MidiInput, MidiInputConnection, MidiInputPort};
use wmidi::MidiMessage;

use crate::action::MidiAction;
use crate::device;
use crate::error::MidiError;
use crate::mapping::{MappingTable, Trigger};

/// A raw MIDI event captured for "learn" mode. Emitted for every
/// CC / Note-On event regardless of whether a binding matched, so
/// the UI can show the user what their controller is sending.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MidiEvent {
    pub trigger: Trigger,
    pub value:   u8,
}

/// Shared, hot-swappable mapping. The callback reads through
/// `ArcSwap::load_full()` on every event, so the UI can publish a new
/// table without restarting the listener.
pub type SharedMapping = Arc<ArcSwap<MappingTable>>;

/// Running connection. Dropping it stops the background thread.
pub struct MidiListener {
    _conn:     MidiInputConnection<()>,
    port_name: String,
}

impl MidiListener {
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// Open the first port whose name contains `needle`, then forward
/// resolved actions on `action_tx` and raw events on `event_tx`.
/// Either sender may be dropped by the caller; sends silently fail.
pub fn start(
    needle: &str,
    mapping: SharedMapping,
    action_tx: mpsc::Sender<MidiAction>,
    event_tx: mpsc::Sender<MidiEvent>,
) -> Result<MidiListener, MidiError> {
    let (midi_in, port): (MidiInput, MidiInputPort) = device::find_port(needle)?;
    let port_name = midi_in
        .port_name(&port)
        .unwrap_or_else(|_| needle.to_string());

    let conn = midi_in.connect(
        &port,
        "arion-midi-in",
        move |_ts, bytes, _| {
            let msg = match MidiMessage::try_from(bytes) {
                Ok(m) => m,
                Err(_) => return,
            };
            let (trigger, value) = match msg {
                MidiMessage::ControlChange(ch, ctrl, val) => (
                    Trigger::Cc {
                        channel:    ch.index(),
                        controller: u8::from(ctrl),
                    },
                    u8::from(val),
                ),
                MidiMessage::NoteOn(ch, note, val) => {
                    let v = u8::from(val);
                    if v == 0 {
                        return;
                    }
                    (
                        Trigger::Note {
                            channel: ch.index(),
                            note:    u8::from(note),
                        },
                        v,
                    )
                }
                _ => return,
            };
            let _ = event_tx.send(MidiEvent { trigger, value });
            let table = mapping.load_full();
            if let Some(action) = table.resolve(trigger, value) {
                let _ = action_tx.send(action);
            } else {
                tracing::trace!(?trigger, value, "midi: unmapped");
            }
        },
        (),
    )?;

    tracing::info!(port = %port_name, "midi: connected");

    Ok(MidiListener {
        _conn: conn,
        port_name,
    })
}
