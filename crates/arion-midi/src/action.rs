use arion_app::{App, AgcPreset, Band, FilterPreset};
use arion_core::WdspMode;

/// A resolved control-surface action targeting [`App`]. Each variant
/// maps one-to-one to a method on `App`; [`apply`] is the only place
/// that touches `App`, so the dispatch is trivial to audit.
#[derive(Debug, Clone)]
pub enum MidiAction {
    /// AF gain on `rx`, clamped to `[0.0, 1.0]`.
    Volume { rx: u8, value: f32 },
    /// Absolute frequency in Hz (rarely used — most controllers should
    /// emit `FreqDelta` instead via `Scale::Relative`).
    FreqAbsolute { rx: u8, hz: u32 },
    /// Add `delta_hz` to the current frequency on `rx`.
    FreqDelta { rx: u8, delta_hz: i32 },
    /// Add `delta_hz` to the current RIT offset on `rx`.
    RitDelta { rx: u8, delta_hz: i32 },
    /// Switch operating mode on `rx`.
    SetMode { rx: u8, mode: WdspMode },
    /// Apply one of the fixed filter-width presets.
    FilterPreset { rx: u8, preset: FilterPreset },
    /// Cycle AGC preset on `rx`.
    SetAgc { rx: u8, agc: AgcPreset },
    /// Jump to a specific ham band.
    JumpBand(Band),
    /// Load memory channel `idx` (no-op if out of range).
    LoadMemory { idx: usize },
    /// Make receiver `rx` the active one.
    ActiveRx { rx: u8 },
    /// Toggle a boolean `App` flag by name. Valid flags mirror
    /// [`App::toggle_rx_flag`] — e.g. `"nr3"`, `"nr4"`, `"anf"`,
    /// `"nb"`, `"nb2"`, `"bin"`, `"tnf"`, `"eq"`, `"mute"`, `"lock"`.
    ToggleFlag { rx: u8, flag: String },
    /// Push-to-talk. No-op while Phase C (TX) is unimplemented —
    /// kept in the enum so bindings persisted today keep working
    /// when TX lands.
    Ptt(bool),
}

impl MidiAction {
    pub fn apply(&self, app: &mut App) {
        match self {
            MidiAction::Volume { rx, value } => {
                app.set_rx_volume(*rx, value.clamp(0.0, 1.0));
            }
            MidiAction::FreqAbsolute { rx, hz } => {
                app.set_rx_frequency(*rx, *hz);
            }
            MidiAction::FreqDelta { rx, delta_hz } => {
                if let Some(state) = app.rx(*rx as usize) {
                    let cur = state.frequency_hz as i64;
                    let next = (cur + *delta_hz as i64).clamp(0, u32::MAX as i64) as u32;
                    app.set_rx_frequency(*rx, next);
                }
            }
            MidiAction::RitDelta { rx, delta_hz } => {
                if let Some(state) = app.rx(*rx as usize) {
                    let cur = state.rit_hz;
                    app.set_rx_rit(*rx, cur.saturating_add(*delta_hz));
                }
            }
            MidiAction::SetMode { rx, mode } => {
                app.set_rx_mode(*rx, *mode);
            }
            MidiAction::FilterPreset { rx, preset } => {
                app.set_rx_filter_preset(*rx, *preset);
            }
            MidiAction::SetAgc { rx, agc } => {
                app.set_rx_agc(*rx, *agc);
            }
            MidiAction::JumpBand(band) => {
                app.jump_to_band(*band);
            }
            MidiAction::LoadMemory { idx } => {
                app.load_memory(*idx);
            }
            MidiAction::ActiveRx { rx } => {
                app.set_active_rx(*rx as usize);
            }
            MidiAction::ToggleFlag { rx, flag } => {
                app.toggle_rx_flag(*rx, flag);
            }
            MidiAction::Ptt(_on) => {
                // Phase C not implemented yet.
                tracing::debug!("midi: PTT ignored (TX not implemented)");
            }
        }
    }
}
