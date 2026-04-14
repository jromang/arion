//! Wire format for the control WebSocket.
//!
//! Server → client: [`Envelope::State`] snapshot (pushed on a timer).
//! Client → server: [`ClientEnvelope::Action`] — apply to the `App`.

use serde::{Deserialize, Serialize};

use arion_app::App;
use arion_core::{Telemetry, WdspMode};

// ---------- server → client ----------

#[derive(Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Envelope<'a> {
    State(StateSnapshot),
    Webrtc(WebrtcServer<'a>),
}

#[derive(Serialize)]
pub struct StateSnapshot {
    pub num_rx: u8,
    pub active_rx: usize,
    pub radio_connected: bool,
    pub rx: Vec<RxSnapshot>,
}

#[derive(Serialize)]
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
}

impl StateSnapshot {
    pub fn from_app_and_telemetry(app: &App, telemetry: &Telemetry) -> Self {
        let rx = app
            .rxs()
            .iter()
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
                }
            })
            .collect();
        StateSnapshot {
            num_rx: app.num_rx(),
            active_rx: app.active_rx(),
            radio_connected: app.is_connected(),
            rx,
        }
    }
}

// ---------- client → server ----------

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

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    SetRxFrequency { rx: u8, hz: u32 },
    SetRxMode      { rx: u8, mode: String },
    SetRxVolume    { rx: u8, volume: f32 },
    ToggleRxFlag   { rx: u8, flag: String },
    SetActiveRx    { rx: usize },
}

impl Action {
    pub fn apply(self, app: &mut App) {
        match self {
            Action::SetRxFrequency { rx, hz } => app.set_rx_frequency(rx, hz),
            Action::SetRxMode { rx, mode } => {
                if let Some(m) = mode_from_label(&mode) {
                    app.set_rx_mode(rx, m);
                }
            }
            Action::SetRxVolume { rx, volume } => app.set_rx_volume(rx, volume),
            Action::ToggleRxFlag { rx, flag } => app.toggle_rx_flag(rx, &flag),
            Action::SetActiveRx { rx } => app.set_active_rx(rx),
        }
    }
}

fn mode_label(m: WdspMode) -> &'static str {
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

fn mode_from_label(s: &str) -> Option<WdspMode> {
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
