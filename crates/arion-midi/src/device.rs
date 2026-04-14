use midir::{MidiInput, MidiInputPort};

use crate::error::MidiError;

/// List the names of all currently visible MIDI input ports. Order is
/// whatever the OS gives us; the first match is used by [`find_port`].
pub fn enum_inputs() -> Result<Vec<String>, MidiError> {
    let midi_in = MidiInput::new("arion-midi-enum")?;
    let mut out = Vec::with_capacity(midi_in.port_count());
    for p in midi_in.ports() {
        if let Ok(name) = midi_in.port_name(&p) {
            out.push(name);
        }
    }
    Ok(out)
}

/// Find a port whose name contains `needle` (case-sensitive substring).
/// Returns the backend handle *together with* an owned [`MidiInput`]
/// because `midir` ports are tied to the input instance they came from.
pub(crate) fn find_port(needle: &str) -> Result<(MidiInput, MidiInputPort), MidiError> {
    let midi_in = MidiInput::new("arion-midi")?;
    for p in midi_in.ports() {
        if let Ok(name) = midi_in.port_name(&p) {
            if name.contains(needle) {
                return Ok((midi_in, p));
            }
        }
    }
    Err(MidiError::DeviceNotFound(needle.to_string()))
}
