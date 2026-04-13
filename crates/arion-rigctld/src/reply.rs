//! Value-object types for rigctld responses.
//!
//! A [`RigReply`] is what a [`RigCommand`](crate::commands::RigCommand)
//! produces when executed against the `App`. The session layer converts
//! it to wire bytes via [`format_reply`](crate::protocol::format_reply).

/// Raw reply from a command, independent of plain/extended wire format.
#[derive(Debug, Clone)]
pub enum RigReply {
    /// Equivalent to `RPRT 0` on the wire.
    Ok,
    /// Error code (negative Hamlib RIG_E* value). `-11` = feature not
    /// available, `-1` = generic error, etc.
    Error(i32),
    /// Multi-line key/value body (used by `\dump_state`, level lists, …).
    /// In plain mode the keys are dropped and only the values are sent,
    /// one per line; in extended mode `Key: Value` lines are sent.
    KeyValues(Vec<(&'static str, String)>),
    /// Plain single-line value (e.g. a frequency in Hz, a mode string).
    Value(String),
    /// Multi-line raw body (inserted verbatim, e.g. `\dump_state`).
    Raw(String),
}
