//! Transport-neutral DTOs describing the radio domain.
//!
//! `StateSnapshot` + `Action` were previously owned by `arion-web`,
//! but they're not web-specific — any external control surface (REST
//! API, future TCI server, etc.) projects the same view of the App
//! state and dispatches the same actions. Hosting them here keeps the
//! types independent from any particular transport crate.

use serde::{Deserialize, Serialize};

use arion_core::{Telemetry, WdspMode};

use crate::{mode_from_serde, mode_to_serde, AgcPreset, App, Band, FilterPreset};

#[derive(Serialize, Clone, Default)]
pub struct StateSnapshot {
    pub num_rx: u8,
    pub active_rx: usize,
    pub radio_connected: bool,
    pub radio_ip: String,
    pub rx: Vec<RxSnapshot>,
    pub memories: Vec<MemorySnapshot>,
}

#[derive(Serialize, Clone)]
pub struct MemorySnapshot {
    pub name:         String,
    pub tag:          String,
    pub frequency_hz: u32,
    pub mode:         &'static str,
}

#[derive(Serialize, Clone)]
pub struct RxSnapshot {
    pub enabled:      bool,
    pub frequency_hz: u32,
    pub mode:         &'static str,
    pub volume:       f32,
    pub s_meter_db:   f32,
    pub nb:           bool,
    pub nb2:          bool,
    pub anf:          bool,
    pub bin:          bool,
    pub tnf:          bool,
    pub nr3:          bool,
    pub nr4:          bool,
    pub anr:          bool,
    pub emnr:         bool,
    pub squelch:         bool,
    pub squelch_db:      f32,
    pub apf:             bool,
    pub apf_freq_hz:     f32,
    pub agc_top_dbm:     f32,
    pub agc_decay_ms:    i32,
    pub fm_deviation_hz: f32,
    pub ctcss_on:        bool,
    pub ctcss_hz:        f32,
}

impl StateSnapshot {
    pub fn from_app_and_telemetry(app: &App, telemetry: &Telemetry) -> Self {
        let rx = app
            .rxs()
            .iter()
            .take(app.num_rx() as usize)
            .enumerate()
            .map(|(i, r)| {
                let s_meter_db = telemetry.rx.get(i).map(|rt| rt.s_meter_db).unwrap_or(-140.0);
                RxSnapshot {
                    enabled:      r.enabled,
                    frequency_hz: r.frequency_hz,
                    mode:         mode_label(r.mode),
                    volume:       r.volume,
                    s_meter_db,
                    nb:           r.nb,
                    nb2:          r.nb2,
                    anf:          r.anf,
                    bin:          r.bin,
                    tnf:          r.tnf,
                    nr3:          r.nr3,
                    nr4:          r.nr4,
                    anr:          r.anr,
                    emnr:         r.emnr,
                    squelch:         r.squelch,
                    squelch_db:      r.squelch_db,
                    apf:             r.apf,
                    apf_freq_hz:     r.apf_freq_hz,
                    agc_top_dbm:     r.agc_top_dbm,
                    agc_decay_ms:    r.agc_decay_ms,
                    fm_deviation_hz: r.fm_deviation_hz,
                    ctcss_on:        r.ctcss_on,
                    ctcss_hz:        r.ctcss_hz,
                }
            })
            .collect();
        let memories = app
            .memories()
            .iter()
            .map(|m| MemorySnapshot {
                name:         m.name.clone(),
                tag:          m.tag.clone(),
                frequency_hz: m.freq_hz,
                mode:         mode_label(mode_from_serde(m.mode)),
            })
            .collect();
        StateSnapshot {
            num_rx: app.num_rx(),
            active_rx: app.active_rx(),
            radio_connected: app.is_connected(),
            radio_ip: app.radio_ip().to_string(),
            rx,
            memories,
        }
    }
}

/// One control-surface action targeting `App`. Same pattern as
/// `arion_midi::MidiAction` and `arion_rigctld::RigCommand`.
///
/// Actions are transport-neutral — the REST API, the control
/// WebSocket, the TCI server (when it lands), any scripting layer,
/// all funnel through this one enum and the `apply` dispatch below.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    // --- RX ---
    SetRxFrequency { rx: u8, hz: u32 },
    TuneRx         { rx: u8, delta_hz: i32 },
    SetRxMode      { rx: u8, mode: String },
    SetRxVolume    { rx: u8, volume: f32 },
    SetRxMuted     { rx: u8, muted: bool },
    SetRxLocked    { rx: u8, locked: bool },
    SetRxRit       { rx: u8, hz: i32 },
    SetRxNr3       { rx: u8, on: bool },
    SetRxNr4       { rx: u8, on: bool },
    SetRxAnr       { rx: u8, on: bool },
    SetRxEmnr      { rx: u8, on: bool },
    SetRxSquelch   { rx: u8, on: bool },
    SetRxSquelchThreshold { rx: u8, db: f32 },
    SetRxApf       { rx: u8, on: bool },
    SetRxApfFreq   { rx: u8, hz: f32 },
    SetRxApfBandwidth { rx: u8, hz: f32 },
    SetRxApfGain   { rx: u8, db: f32 },
    SetRxAgcTop    { rx: u8, dbm: f32 },
    SetRxAgcHangLevel { rx: u8, level: f32 },
    SetRxAgcDecay  { rx: u8, ms: i32 },
    SetRxAgcFixedGain { rx: u8, db: f32 },
    SetRxFmDeviation { rx: u8, hz: f32 },
    SetRxCtcss     { rx: u8, on: bool },
    SetRxCtcssFreq { rx: u8, hz: f32 },
    AddRxTnfNotch    { rx: u8, freq_hz: f64, width_hz: f64, active: bool },
    EditRxTnfNotch   { rx: u8, idx: u32, freq_hz: f64, width_hz: f64, active: bool },
    DeleteRxTnfNotch { rx: u8, idx: u32 },
    SetRxSamSubmode  { rx: u8, submode: u8 },
    SetRxAgc       { rx: u8, agc: String },
    SetRxFilter    { rx: u8, low: f64, high: f64 },
    SetRxFilterPreset { rx: u8, preset: String },
    SetRxEq        { rx: u8, gains: Vec<i32> },
    ToggleRxFlag   { rx: u8, flag: String },
    SetActiveRx    { rx: usize },

    // --- Bands / memories / radio ---
    JumpBand       { band: String },
    LoadMemory     { idx: usize },
    SaveMemory     { rx: u8, name: String, tag: String },
    DeleteMemory   { idx: usize },
    UpdateMemory   { idx: usize, name: String, tag: String, frequency_hz: u32, mode: String },
    RadioConnect   { ip: Option<String> },
    RadioDisconnect,

    // --- Services ---
    SetRigctldEnabled { enabled: bool, port: Option<u16> },
    SetMidiEnabled    { enabled: bool, device_name: Option<String> },
}

impl Action {
    pub fn apply(self, app: &mut App) {
        match self {
            Action::SetRxFrequency { rx, hz } => app.set_rx_frequency(rx, hz),
            Action::TuneRx { rx, delta_hz } => {
                if let Some(st) = app.rx(rx as usize) {
                    let next = (st.frequency_hz as i64 + delta_hz as i64)
                        .clamp(0, u32::MAX as i64) as u32;
                    app.set_rx_frequency(rx, next);
                }
            }
            Action::SetRxMode { rx, mode } => {
                if let Some(m) = mode_from_label(&mode) {
                    app.set_rx_mode(rx, m);
                }
            }
            Action::SetRxVolume { rx, volume } => app.set_rx_volume(rx, volume),
            Action::SetRxMuted { rx, muted } => app.set_rx_muted(rx, muted),
            Action::SetRxLocked { rx, locked } => app.set_rx_locked(rx, locked),
            Action::SetRxRit { rx, hz } => app.set_rx_rit(rx, hz),
            Action::SetRxNr3 { rx, on } => app.set_rx_nr3(rx, on),
            Action::SetRxNr4 { rx, on } => app.set_rx_nr4(rx, on),
            Action::SetRxAnr { rx, on } => app.set_rx_anr(rx, on),
            Action::SetRxEmnr { rx, on } => app.set_rx_emnr(rx, on),
            Action::SetRxSquelch { rx, on } => app.set_rx_squelch(rx, on),
            Action::SetRxSquelchThreshold { rx, db } => app.set_rx_squelch_threshold(rx, db),
            Action::SetRxApf { rx, on } => app.set_rx_apf(rx, on),
            Action::SetRxApfFreq { rx, hz } => app.set_rx_apf_freq(rx, hz),
            Action::SetRxApfBandwidth { rx, hz } => app.set_rx_apf_bandwidth(rx, hz),
            Action::SetRxApfGain { rx, db } => app.set_rx_apf_gain(rx, db),
            Action::SetRxAgcTop { rx, dbm } => app.set_rx_agc_top(rx, dbm),
            Action::SetRxAgcHangLevel { rx, level } => app.set_rx_agc_hang_level(rx, level),
            Action::SetRxAgcDecay { rx, ms } => app.set_rx_agc_decay(rx, ms),
            Action::SetRxAgcFixedGain { rx, db } => app.set_rx_agc_fixed_gain(rx, db),
            Action::SetRxFmDeviation { rx, hz } => app.set_rx_fm_deviation(rx, hz),
            Action::SetRxCtcss { rx, on } => app.set_rx_ctcss(rx, on),
            Action::SetRxCtcssFreq { rx, hz } => app.set_rx_ctcss_freq(rx, hz),
            Action::AddRxTnfNotch { rx, freq_hz, width_hz, active } => {
                app.add_rx_tnf_notch(rx, freq_hz, width_hz, active);
            }
            Action::EditRxTnfNotch { rx, idx, freq_hz, width_hz, active } => {
                app.edit_rx_tnf_notch(rx, idx, freq_hz, width_hz, active);
            }
            Action::DeleteRxTnfNotch { rx, idx } => app.delete_rx_tnf_notch(rx, idx),
            Action::SetRxSamSubmode { rx, submode } => app.set_rx_sam_submode(rx, submode),
            Action::SetRxAgc { rx, agc } => {
                if let Some(a) = agc_from_label(&agc) {
                    app.set_rx_agc(rx, a);
                }
            }
            Action::SetRxFilter { rx, low, high } => app.set_rx_filter(rx, low, high),
            Action::SetRxFilterPreset { rx, preset } => {
                if let Some(p) = filter_preset_from_label(&preset) {
                    app.set_rx_filter_preset(rx, p);
                }
            }
            Action::SetRxEq { rx, gains } => {
                if gains.len() == 11 {
                    let mut arr = [0i32; 11];
                    arr.copy_from_slice(&gains);
                    app.set_rx_eq_gains(rx, arr);
                }
            }
            Action::ToggleRxFlag { rx, flag } => app.toggle_rx_flag(rx, &flag),
            Action::SetActiveRx { rx } => app.set_active_rx(rx),

            Action::JumpBand { band } => {
                if let Some(b) = band_from_label(&band) {
                    app.jump_to_band(b);
                }
            }
            Action::LoadMemory { idx } => app.load_memory(idx),
            Action::SaveMemory { rx, name, tag } => {
                if let Some(st) = app.rx(rx as usize) {
                    let mem = arion_settings::Memory {
                        name,
                        tag,
                        freq_hz: st.frequency_hz,
                        mode:    mode_to_serde(st.mode),
                    };
                    app.add_memory(mem);
                }
            }
            Action::DeleteMemory { idx } => app.delete_memory(idx),
            Action::UpdateMemory { idx, name, tag, frequency_hz, mode } => {
                if let Some(m) = mode_from_label(&mode) {
                    let mem = arion_settings::Memory {
                        name,
                        tag,
                        freq_hz: frequency_hz,
                        mode:    mode_to_serde(m),
                    };
                    app.delete_memory(idx);
                    app.add_memory(mem);
                }
            }
            Action::RadioConnect { ip } => {
                if let Some(ip) = ip {
                    app.set_radio_ip(ip);
                }
                app.connect();
            }
            Action::RadioDisconnect => app.disconnect(),

            Action::SetRigctldEnabled { enabled, port } => {
                let net = app.network_settings_mut();
                net.rigctld_enabled = enabled;
                if let Some(p) = port {
                    net.rigctld_port = p;
                }
            }
            Action::SetMidiEnabled { enabled, device_name } => {
                let midi = app.midi_settings_mut();
                midi.enabled = enabled;
                if device_name.is_some() {
                    midi.device_name = device_name;
                }
            }
        }
    }
}

pub fn mode_label(m: WdspMode) -> &'static str {
    match m {
        WdspMode::Lsb  => "LSB",
        WdspMode::Usb  => "USB",
        WdspMode::Dsb  => "DSB",
        WdspMode::CwL  => "CWL",
        WdspMode::CwU  => "CWU",
        WdspMode::Fm   => "FM",
        WdspMode::Am   => "AM",
        WdspMode::DigU => "DIGU",
        WdspMode::Spec => "SPEC",
        WdspMode::DigL => "DIGL",
        WdspMode::Sam  => "SAM",
        WdspMode::Drm  => "DRM",
    }
}

pub fn mode_from_label(s: &str) -> Option<WdspMode> {
    Some(match s {
        "LSB"  => WdspMode::Lsb,
        "USB"  => WdspMode::Usb,
        "DSB"  => WdspMode::Dsb,
        "CWL"  => WdspMode::CwL,
        "CWU"  => WdspMode::CwU,
        "FM"   => WdspMode::Fm,
        "AM"   => WdspMode::Am,
        "DIGU" => WdspMode::DigU,
        "SPEC" => WdspMode::Spec,
        "DIGL" => WdspMode::DigL,
        "SAM"  => WdspMode::Sam,
        "DRM"  => WdspMode::Drm,
        _ => return None,
    })
}

pub fn agc_label(a: AgcPreset) -> &'static str {
    match a {
        AgcPreset::Off  => "off",
        AgcPreset::Long => "long",
        AgcPreset::Slow => "slow",
        AgcPreset::Med  => "med",
        AgcPreset::Fast => "fast",
    }
}

pub fn agc_from_label(s: &str) -> Option<AgcPreset> {
    Some(match s.to_ascii_lowercase().as_str() {
        "off"  => AgcPreset::Off,
        "long" => AgcPreset::Long,
        "slow" => AgcPreset::Slow,
        "med" | "medium" => AgcPreset::Med,
        "fast" => AgcPreset::Fast,
        _ => return None,
    })
}

pub fn band_label(b: Band) -> &'static str {
    match b {
        Band::M160 => "M160",
        Band::M80  => "M80",
        Band::M60  => "M60",
        Band::M40  => "M40",
        Band::M30  => "M30",
        Band::M20  => "M20",
        Band::M17  => "M17",
        Band::M15  => "M15",
        Band::M12  => "M12",
        Band::M10  => "M10",
        Band::M6   => "M6",
    }
}

pub fn band_from_label(s: &str) -> Option<Band> {
    Some(match s {
        "M160" => Band::M160,
        "M80"  => Band::M80,
        "M60"  => Band::M60,
        "M40"  => Band::M40,
        "M30"  => Band::M30,
        "M20"  => Band::M20,
        "M17"  => Band::M17,
        "M15"  => Band::M15,
        "M12"  => Band::M12,
        "M10"  => Band::M10,
        "M6"   => Band::M6,
        _ => return None,
    })
}

pub fn filter_preset_label(p: FilterPreset) -> &'static str {
    match p {
        FilterPreset::F6000 => "F6000",
        FilterPreset::F4000 => "F4000",
        FilterPreset::F2700 => "F2700",
        FilterPreset::F2400 => "F2400",
        FilterPreset::F1800 => "F1800",
        FilterPreset::F1000 => "F1000",
        FilterPreset::F600  => "F600",
        FilterPreset::F400  => "F400",
        FilterPreset::F250  => "F250",
        FilterPreset::F100  => "F100",
    }
}

pub fn filter_preset_from_label(s: &str) -> Option<FilterPreset> {
    Some(match s {
        "F6000" => FilterPreset::F6000,
        "F4000" => FilterPreset::F4000,
        "F2700" => FilterPreset::F2700,
        "F2400" => FilterPreset::F2400,
        "F1800" => FilterPreset::F1800,
        "F1000" => FilterPreset::F1000,
        "F600"  => FilterPreset::F600,
        "F400"  => FilterPreset::F400,
        "F250"  => FilterPreset::F250,
        "F100"  => FilterPreset::F100,
        _ => return None,
    })
}
