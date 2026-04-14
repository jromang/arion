use arion_app::{AgcPreset, Band, FilterPreset};
use arion_core::WdspMode;
use serde::{Deserialize, Serialize};

use crate::action::MidiAction;

/// What incoming MIDI event triggers a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Cc { channel: u8, controller: u8 },
    Note { channel: u8, note: u8 },
}

/// How the 7-bit MIDI value is interpreted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scale {
    /// Absolute pot: `0..=127` → linearly mapped to `[min, max]`.
    Absolute { min: f32, max: f32 },
    /// Endless encoder. Most controllers send `1` (CW) or `65` (CCW)
    /// by default, but the high bit may also encode speed. We use the
    /// "two's-complement" convention from Mackie Control: values in
    /// `1..=63` are positive deltas (magnitude = value), values in
    /// `65..=127` are negative deltas (magnitude = value - 64).
    Relative { step: f32 },
    /// Note / button: emits the action on Note On regardless of value.
    Trigger,
}

/// The target parameter a binding drives. Enum rather than a function
/// pointer so bindings can be serialised to TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    Volume { rx: u8 },
    Frequency { rx: u8 },
    Rit { rx: u8 },
    /// Selects a mode — value is interpreted as an index into a preset
    /// list, or the Trigger scale fires a single mode directly.
    Mode { rx: u8, mode: WdspMode },
    FilterPreset { rx: u8, preset: FilterPreset },
    Agc { rx: u8, agc: AgcPreset },
    Band { band: Band },
    Memory { idx: usize },
    ActiveRx { rx: u8 },
    ToggleFlag { rx: u8, flag: String },
    Ptt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub trigger: Trigger,
    pub scale:   Scale,
    pub target:  Target,
}

impl Binding {
    /// Resolve an incoming `(trigger, value)` pair to a concrete
    /// [`MidiAction`]. Returns `None` if the trigger doesn't match or
    /// the scale/target combination is meaningless (e.g. Relative on
    /// Band jump).
    pub fn resolve(&self, trigger: Trigger, value: u8) -> Option<MidiAction> {
        if trigger != self.trigger {
            return None;
        }
        match (&self.scale, &self.target) {
            (Scale::Absolute { min, max }, Target::Volume { rx }) => {
                let v = lerp(*min, *max, value);
                Some(MidiAction::Volume { rx: *rx, value: v })
            }
            (Scale::Absolute { min, max }, Target::Frequency { rx }) => {
                let hz = lerp(*min, *max, value) as u32;
                Some(MidiAction::FreqAbsolute { rx: *rx, hz })
            }
            (Scale::Relative { step }, Target::Frequency { rx }) => {
                let delta = relative_delta(value);
                Some(MidiAction::FreqDelta {
                    rx: *rx,
                    delta_hz: (delta as f32 * *step) as i32,
                })
            }
            (Scale::Relative { step }, Target::Rit { rx }) => {
                let delta = relative_delta(value);
                Some(MidiAction::RitDelta {
                    rx: *rx,
                    delta_hz: (delta as f32 * *step) as i32,
                })
            }
            (Scale::Relative { step }, Target::Volume { rx }) => {
                let delta = relative_delta(value);
                Some(MidiAction::Volume {
                    rx: *rx,
                    // Caller feeds back through App clamp — we emit
                    // an absolute value seeded from 0.5, which is the
                    // safest fallback when no read-back is wired yet.
                    value: 0.5 + (delta as f32 * *step),
                })
            }
            (Scale::Trigger, Target::Mode { rx, mode }) => {
                Some(MidiAction::SetMode { rx: *rx, mode: *mode })
            }
            (Scale::Trigger, Target::FilterPreset { rx, preset }) => {
                Some(MidiAction::FilterPreset { rx: *rx, preset: *preset })
            }
            (Scale::Trigger, Target::Agc { rx, agc }) => {
                Some(MidiAction::SetAgc { rx: *rx, agc: *agc })
            }
            (Scale::Trigger, Target::Band { band }) => Some(MidiAction::JumpBand(*band)),
            (Scale::Trigger, Target::Memory { idx }) => {
                Some(MidiAction::LoadMemory { idx: *idx })
            }
            (Scale::Trigger, Target::ActiveRx { rx }) => Some(MidiAction::ActiveRx { rx: *rx }),
            (Scale::Trigger, Target::ToggleFlag { rx, flag }) => Some(MidiAction::ToggleFlag {
                rx:   *rx,
                flag: flag.clone(),
            }),
            (Scale::Trigger, Target::Ptt) => Some(MidiAction::Ptt(true)),
            _ => None,
        }
    }
}

fn lerp(min: f32, max: f32, value: u8) -> f32 {
    let t = f32::from(value) / 127.0;
    min + (max - min) * t
}

/// Decode a 2's-complement-ish relative encoder tick. `1..=63` → `+n`,
/// `65..=127` → `-(n - 64)`, anything else → `0`.
fn relative_delta(value: u8) -> i8 {
    match value {
        1..=63 => value as i8,
        65..=127 => -((value - 64) as i8),
        _ => 0,
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MappingTable {
    pub bindings: Vec<Binding>,
}

impl MappingTable {
    pub fn resolve(&self, trigger: Trigger, value: u8) -> Option<MidiAction> {
        self.bindings.iter().find_map(|b| b.resolve(trigger, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_volume_maps_endpoints() {
        let b = Binding {
            trigger: Trigger::Cc { channel: 0, controller: 7 },
            scale:   Scale::Absolute { min: 0.0, max: 1.0 },
            target:  Target::Volume { rx: 0 },
        };
        let got = b.resolve(Trigger::Cc { channel: 0, controller: 7 }, 0).unwrap();
        assert!(matches!(got, MidiAction::Volume { value, .. } if (value - 0.0).abs() < 1e-6));
        let got = b.resolve(Trigger::Cc { channel: 0, controller: 7 }, 127).unwrap();
        assert!(matches!(got, MidiAction::Volume { value, .. } if (value - 1.0).abs() < 1e-6));
    }

    #[test]
    fn relative_frequency_delta() {
        let b = Binding {
            trigger: Trigger::Cc { channel: 0, controller: 16 },
            scale:   Scale::Relative { step: 10.0 },
            target:  Target::Frequency { rx: 0 },
        };
        // +1 tick → +10 Hz
        let a = b.resolve(Trigger::Cc { channel: 0, controller: 16 }, 1).unwrap();
        assert!(matches!(a, MidiAction::FreqDelta { delta_hz: 10, .. }));
        // -1 tick (value 65) → -10 Hz
        let a = b.resolve(Trigger::Cc { channel: 0, controller: 16 }, 65).unwrap();
        assert!(matches!(a, MidiAction::FreqDelta { delta_hz: -10, .. }));
        // bigger step: value 3 → +30 Hz
        let a = b.resolve(Trigger::Cc { channel: 0, controller: 16 }, 3).unwrap();
        assert!(matches!(a, MidiAction::FreqDelta { delta_hz: 30, .. }));
    }

    #[test]
    fn trigger_scale_fires_band() {
        let b = Binding {
            trigger: Trigger::Note { channel: 0, note: 36 },
            scale:   Scale::Trigger,
            target:  Target::Band { band: Band::M20 },
        };
        let a = b.resolve(Trigger::Note { channel: 0, note: 36 }, 127).unwrap();
        assert!(matches!(a, MidiAction::JumpBand(Band::M20)));
    }

    #[test]
    fn trigger_mismatch_returns_none() {
        let b = Binding {
            trigger: Trigger::Cc { channel: 0, controller: 7 },
            scale:   Scale::Absolute { min: 0.0, max: 1.0 },
            target:  Target::Volume { rx: 0 },
        };
        assert!(b.resolve(Trigger::Cc { channel: 1, controller: 7 }, 64).is_none());
        assert!(b.resolve(Trigger::Note { channel: 0, note: 7 }, 64).is_none());
    }

    #[test]
    fn toml_roundtrip() {
        let t = MappingTable {
            bindings: vec![
                Binding {
                    trigger: Trigger::Cc { channel: 0, controller: 7 },
                    scale:   Scale::Absolute { min: 0.0, max: 1.0 },
                    target:  Target::Volume { rx: 0 },
                },
                Binding {
                    trigger: Trigger::Note { channel: 0, note: 36 },
                    scale:   Scale::Trigger,
                    target:  Target::Band { band: Band::M20 },
                },
            ],
        };
        let s = toml::to_string(&t).unwrap();
        let back: MappingTable = toml::from_str(&s).unwrap();
        assert_eq!(back.bindings.len(), 2);
    }
}
