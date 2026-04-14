//! MIDI controller bridge for Arion.
//!
//! Reads MIDI input from a hardware controller, resolves CC/Note
//! events through a [`MappingTable`], and forwards the resulting
//! [`MidiAction`]s on an `mpsc::Sender` for the UI thread to apply.
//!
//! Threading model mirrors [`arion_rigctld`]: the `midir` callback
//! thread pushes actions into a channel and never touches `App`
//! directly. The UI thread owns `App` and drains the channel once
//! per frame via [`drain`].
//!
//! ```text
//!   MIDI device ─▶ midir callback ─┬─▶ mpsc::Sender<MidiAction> ─▶ UI drain
//!                                  └─▶ mpsc::Sender<MidiEvent>  ─▶ Learn UI
//! ```
//!
//! The mapping table is behind [`Arc<ArcSwap<_>>`](SharedMapping)
//! so the UI can swap bindings at runtime without restarting the
//! backend thread.

#![forbid(unsafe_code)]

use std::sync::{mpsc, Arc};

use arc_swap::ArcSwap;
use arion_app::App;

pub mod action;
pub mod device;
pub mod error;
pub mod listener;
pub mod mapping;
pub mod persist;

pub use action::MidiAction;
pub use error::MidiError;
pub use listener::{start, MidiEvent, MidiListener, SharedMapping};
pub use mapping::{Binding, MappingTable, Scale, Target, Trigger};

/// Wrap a mapping in the hot-swappable container accepted by
/// [`listener::start`].
pub fn shared(m: MappingTable) -> SharedMapping {
    Arc::new(ArcSwap::new(Arc::new(m)))
}

/// Baseline mapping for smoke tests: CC 7 (Channel Volume, any
/// channel-0 controller) → RX0 AF gain. Kept simple so a brand-new
/// controller drives *something* on first plug without user setup.
pub fn default_mapping() -> MappingTable {
    MappingTable {
        bindings: vec![Binding {
            trigger: Trigger::Cc { channel: 0, controller: 7 },
            scale:   Scale::Absolute { min: 0.0, max: 1.0 },
            target:  Target::Volume { rx: 0 },
        }],
    }
}

/// Apply up to `max_per_frame` pending actions against `app`. Called
/// once per UI frame. Silent on an empty channel.
pub fn drain(app: &mut App, rx: &mpsc::Receiver<MidiAction>) {
    drain_with_limit(app, rx, 64);
}

pub fn drain_with_limit(app: &mut App, rx: &mpsc::Receiver<MidiAction>, max_per_frame: usize) {
    for _ in 0..max_per_frame {
        match rx.try_recv() {
            Ok(a) => a.apply(app),
            Err(_) => break,
        }
    }
}
