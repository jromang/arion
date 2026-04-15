//! Headless view-model layer for Arion.
//!
//! This crate is the application's **view-model** in our MVVM split.
//! It owns every piece of state that survives across frames — the
//! optional [`Radio`] handle, the per-RX form fields, the band stack,
//! the memories list, the dirty/save bookkeeping, the active-RX
//! cursor — and exposes a small read/write API that the frontends
//! ([`arion-egui`] desktop, [`arion-tui`] console, soon also the
//! Rhai scripting layer in phase D.12) consume.
//!
//! **Hard rule**: zero dependency on any UI framework. No `egui`,
//! `eframe`, `ratatui`, `crossterm`, `wgpu`. The crate must compile
//! and unit-test on a headless server with `cargo test -p arion-app`.
//!
//! The frontends are *humble views*: they read from `App` immutable
//! getters, dispatch user actions through `App::set_*` / `App::toggle_*`,
//! and otherwise render whatever's in scope. The single-source-of-truth
//! for "what should the screen look like" lives here.

pub mod protocol;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use arion_core::{Radio, RadioConfig, RxConfig, Telemetry, WdspMode, MAX_RX};
use arion_settings::{
    BandStackEntry as SerdeBandStackEntry, GeneralSettings, Memory, Mode as SerdeMode,
    RxSettings as SerdeRxSettings, Settings,
};

// --------------------------------------------------------------------
// Constants
// --------------------------------------------------------------------

/// Minimum interval between background TOML writes during a live
/// session. Quitting / disconnecting always saves immediately,
/// regardless of this debounce.
pub const SAVE_DEBOUNCE: Duration = Duration::from_secs(10);

/// dBFS → dBm calibration assuming `S9 = -73 dBm` at 50 Ω. Phase B
/// placeholder; phase D.10 replaces it with a per-band table stored
/// in `arion-settings::Calibration`.
pub const SMETER_DBFS_TO_DBM_OFFSET: f32 = 73.0;

// --------------------------------------------------------------------
// Window enum (frontend-agnostic)
// --------------------------------------------------------------------

/// One identifier per floating window the frontend may show. Lives in
/// the view-model so the show/hide state is shared between egui and
/// TUI (a script that opens "Memories" works in both frontends).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowKind {
    Memories,
    BandStack,
    Multimeter,
    Setup,
    Repl,
    Eq,
}

// --------------------------------------------------------------------
// Per-RX state (no view-tied fields like waterfall textures)
// --------------------------------------------------------------------

/// Per-RX state owned by [`App`]. The frontend's per-RX waterfall
/// texture cache (egui `TextureHandle`) lives in the frontend struct
/// alongside `App`, NOT here.
#[derive(Debug, Clone)]
pub struct RxState {
    pub enabled:      bool,
    pub frequency_hz: u32,
    pub mode:         WdspMode,
    pub volume:       f32,
    pub nr3:          bool,
    pub nr4:          bool,
    pub anr:          bool,
    pub emnr:         bool,
    // --- E.10–E.13 (squelch / APF / AGC fine / FM) ---
    pub squelch:         bool,
    pub squelch_db:      f32,
    pub apf:             bool,
    pub apf_freq_hz:     f32,
    pub apf_bw_hz:       f32,
    pub apf_gain_db:     f32,
    pub agc_top_dbm:     f32,
    pub agc_hang_level:  f32,
    pub agc_decay_ms:    i32,
    pub agc_fixed_gain:  f32,
    pub fm_deviation_hz: f32,
    pub ctcss_on:        bool,
    pub ctcss_hz:        f32,
    pub filter_lo:    f64,
    pub filter_hi:    f64,
    // --- DSP toggles (UI + persist, DSP binding in E) ---
    pub agc_mode:     AgcPreset,
    pub muted:        bool,
    pub locked:       bool,
    pub nb:           bool,
    pub nb2:          bool,
    pub anf:          bool,
    pub bin:          bool,
    pub tnf:          bool,
    /// Receiver Incremental Tuning offset in Hz (display-only for now).
    /// Positive = receive above the VFO, negative = below. Zero hides
    /// the on-spectrum marker.
    pub rit_hz:       i32,
    pub eq_enabled:   bool,
    /// 10-band graphic EQ gains. Index 0 = preamp, 1..=10 = bands
    /// at 32, 63, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz.
    /// Values in dB, typically -12..+12.
    pub eq_gains:     [i32; 11],
}

impl Default for RxState {
    fn default() -> Self {
        let (lo, hi) = WdspMode::Usb.default_passband_hz();
        RxState {
            enabled:      false,
            frequency_hz: 7_074_000,
            mode:         WdspMode::Usb,
            volume:       0.25,
            nr3:          false,
            nr4:          false,
            anr:          false,
            emnr:         false,
            squelch:         false,
            squelch_db:      -30.0,
            apf:             false,
            apf_freq_hz:     600.0,
            apf_bw_hz:       50.0,
            apf_gain_db:     6.0,
            agc_top_dbm:     -30.0,
            agc_hang_level:  -20.0,
            agc_decay_ms:    250,
            agc_fixed_gain:  10.0,
            fm_deviation_hz: 5000.0,
            ctcss_on:        false,
            ctcss_hz:        67.0,
            filter_lo:    lo,
            filter_hi:    hi,
            agc_mode:     AgcPreset::Med,
            muted:        false,
            locked:       false,
            nb:           false,
            nb2:          false,
            anf:          false,
            bin:          false,
            tnf:          false,
            rit_hz:       0,
            eq_enabled:   false,
            eq_gains:     [0; 11],
        }
    }
}

/// AGC speed presets matching Arion upstream's combo. Wire-level
/// binding to `wdsp::AgcMode` happens in phase E; for now this is
/// UI state only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AgcPreset {
    Off,
    Long,
    Slow,
    #[default]
    Med,
    Fast,
}

/// Named filter bandwidth presets matching Arion upstream's 10-button
/// row (Filter1..Filter10). Values are passband width in Hz for
/// SSB-like modes; the actual low/high depends on mode (USB → positive,
/// LSB → negative, AM → symmetric).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FilterPreset {
    F6000,
    F4000,
    F2700,
    F2400,
    F1800,
    F1000,
    F600,
    F400,
    F250,
    F100,
}

impl FilterPreset {
    pub const ALL: [FilterPreset; 10] = [
        Self::F6000, Self::F4000, Self::F2700, Self::F2400, Self::F1800,
        Self::F1000, Self::F600,  Self::F400,  Self::F250,  Self::F100,
    ];

    pub fn width_hz(self) -> f64 {
        match self {
            Self::F6000 => 6000.0,
            Self::F4000 => 4000.0,
            Self::F2700 => 2700.0,
            Self::F2400 => 2400.0,
            Self::F1800 => 1800.0,
            Self::F1000 => 1000.0,
            Self::F600  =>  600.0,
            Self::F400  =>  400.0,
            Self::F250  =>  250.0,
            Self::F100  =>  100.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::F6000 => "6.0K",
            Self::F4000 => "4.0K",
            Self::F2700 => "2.7K",
            Self::F2400 => "2.4K",
            Self::F1800 => "1.8K",
            Self::F1000 => "1.0K",
            Self::F600  => "600",
            Self::F400  => "400",
            Self::F250  => "250",
            Self::F100  => "100",
        }
    }

    /// Compute (lo, hi) passband Hz for this preset in the given mode.
    /// USB/DigU: 200..200+width, LSB/DigL: -(200+width)..-200,
    /// AM/SAM/FM/DSB: symmetric ±width/2, CW: centered on pitch (650 Hz).
    pub fn passband_for_mode(self, mode: WdspMode) -> (f64, f64) {
        let w = self.width_hz();
        match mode {
            WdspMode::Usb | WdspMode::DigU => (200.0, 200.0 + w),
            WdspMode::Lsb | WdspMode::DigL => (-(200.0 + w), -200.0),
            WdspMode::CwU => {
                let center = 650.0;
                (center - w / 2.0, center + w / 2.0)
            }
            WdspMode::CwL => {
                let center = -650.0;
                (center - w / 2.0, center + w / 2.0)
            }
            _ => (-w / 2.0, w / 2.0),
        }
    }
}

// --------------------------------------------------------------------
// Mode <-> SerdeMode adapters (centralized so the conversion isn't
// scattered across frontends + future scripting layer)
// --------------------------------------------------------------------

pub fn mode_to_serde(m: WdspMode) -> SerdeMode {
    match m {
        WdspMode::Lsb  => SerdeMode::Lsb,
        WdspMode::Usb  => SerdeMode::Usb,
        WdspMode::Dsb  => SerdeMode::Dsb,
        WdspMode::CwL  => SerdeMode::CwL,
        WdspMode::CwU  => SerdeMode::CwU,
        WdspMode::Fm   => SerdeMode::Fm,
        WdspMode::Am   => SerdeMode::Am,
        WdspMode::DigU => SerdeMode::DigU,
        WdspMode::Spec => SerdeMode::Spec,
        WdspMode::DigL => SerdeMode::DigL,
        WdspMode::Sam  => SerdeMode::Sam,
        WdspMode::Drm  => SerdeMode::Drm,
    }
}

pub fn mode_from_serde(m: SerdeMode) -> WdspMode {
    match m {
        SerdeMode::Lsb  => WdspMode::Lsb,
        SerdeMode::Usb  => WdspMode::Usb,
        SerdeMode::Dsb  => WdspMode::Dsb,
        SerdeMode::CwL  => WdspMode::CwL,
        SerdeMode::CwU  => WdspMode::CwU,
        SerdeMode::Fm   => WdspMode::Fm,
        SerdeMode::Am   => WdspMode::Am,
        SerdeMode::DigU => WdspMode::DigU,
        SerdeMode::Spec => WdspMode::Spec,
        SerdeMode::DigL => WdspMode::DigL,
        SerdeMode::Sam  => WdspMode::Sam,
        SerdeMode::Drm  => WdspMode::Drm,
    }
}

// --------------------------------------------------------------------
// Amateur bands (HF + 6m) — used by Band buttons + band stack
// --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Band {
    M160, M80, M60, M40, M30, M20, M17, M15, M12, M10, M6,
}

impl Band {
    pub const ALL: [Band; 11] = [
        Band::M160, Band::M80, Band::M60, Band::M40, Band::M30,
        Band::M20,  Band::M17, Band::M15, Band::M12, Band::M10, Band::M6,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Band::M160 => "160", Band::M80 => "80", Band::M60 => "60",
            Band::M40  => "40",  Band::M30 => "30", Band::M20 => "20",
            Band::M17  => "17",  Band::M15 => "15", Band::M12 => "12",
            Band::M10  => "10",  Band::M6  => "6",
        }
    }

    /// Inclusive frequency range covered by the band, in Hz. Used to
    /// match the active VFO back to a band when the user moves between
    /// bands so the band-stack snapshot lands in the right slot.
    pub fn range_hz(self) -> (u32, u32) {
        match self {
            Band::M160 => ( 1_800_000,  2_000_000),
            Band::M80  => ( 3_500_000,  4_000_000),
            Band::M60  => ( 5_330_000,  5_410_000),
            Band::M40  => ( 7_000_000,  7_300_000),
            Band::M30  => (10_100_000, 10_150_000),
            Band::M20  => (14_000_000, 14_350_000),
            Band::M17  => (18_068_000, 18_168_000),
            Band::M15  => (21_000_000, 21_450_000),
            Band::M12  => (24_890_000, 24_990_000),
            Band::M10  => (28_000_000, 29_700_000),
            Band::M6   => (50_000_000, 54_000_000),
        }
    }

    pub fn for_freq(freq_hz: u32) -> Option<Band> {
        Band::ALL.iter().copied().find(|b| {
            let (lo, hi) = b.range_hz();
            (lo..=hi).contains(&freq_hz)
        })
    }

    /// Default frequency + mode used when the user presses a band
    /// button for the first time. Anchored on FT8 frequencies for HF
    /// bands (where activity is highest in 2026), 60m and 160m on
    /// classic phone spots.
    pub fn default_entry(self) -> BandStackEntry {
        let (freq, mode) = match self {
            Band::M160 => ( 1_840_000, WdspMode::Lsb),
            Band::M80  => ( 3_573_000, WdspMode::Usb),
            Band::M60  => ( 5_357_000, WdspMode::Usb),
            Band::M40  => ( 7_074_000, WdspMode::Usb),
            Band::M30  => (10_136_000, WdspMode::Usb),
            Band::M20  => (14_074_000, WdspMode::Usb),
            Band::M17  => (18_100_000, WdspMode::Usb),
            Band::M15  => (21_074_000, WdspMode::Usb),
            Band::M12  => (24_915_000, WdspMode::Usb),
            Band::M10  => (28_074_000, WdspMode::Usb),
            Band::M6   => (50_313_000, WdspMode::Usb),
        };
        BandStackEntry { frequency_hz: freq, mode }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BandStackEntry {
    pub frequency_hz: u32,
    pub mode:         WdspMode,
}

/// One slot per [`Band`] holding the user's last freq/mode on that
/// band. Storage is a fixed-length array so lookup is O(1) and the
/// type stays serialisable.
#[derive(Debug, Clone)]
pub struct BandStack {
    entries: [BandStackEntry; Band::ALL.len()],
}

impl Default for BandStack {
    fn default() -> Self {
        let mut entries = [BandStackEntry {
            frequency_hz: 0,
            mode:         WdspMode::Usb,
        }; Band::ALL.len()];
        for (i, b) in Band::ALL.iter().enumerate() {
            entries[i] = b.default_entry();
        }
        BandStack { entries }
    }
}

impl BandStack {
    pub fn get(&self, band: Band) -> BandStackEntry {
        self.entries[Band::ALL.iter().position(|b| *b == band).unwrap()]
    }
    pub fn set(&mut self, band: Band, entry: BandStackEntry) {
        let idx = Band::ALL.iter().position(|b| *b == band).unwrap();
        self.entries[idx] = entry;
    }

    /// Reconstruct a `BandStack` from a `[band_stacks]` table loaded
    /// from `arion.toml`. Missing entries fall back to the band's
    /// hard-coded default (FT8 anchors / classic phone spots).
    pub fn from_settings(map: &std::collections::BTreeMap<String, SerdeBandStackEntry>) -> Self {
        let mut stack = BandStack::default();
        for band in Band::ALL {
            if let Some(entry) = map.get(band.label()) {
                stack.set(band, BandStackEntry {
                    frequency_hz: entry.frequency_hz,
                    mode:         mode_from_serde(entry.mode),
                });
            }
        }
        stack
    }

    /// Serialise to the on-disk `BTreeMap` representation. Sorted
    /// keys keep diffs stable across saves.
    pub fn to_settings(&self) -> std::collections::BTreeMap<String, SerdeBandStackEntry> {
        let mut out = std::collections::BTreeMap::new();
        for band in Band::ALL {
            let entry = self.get(band);
            out.insert(
                band.label().to_string(),
                SerdeBandStackEntry {
                    frequency_hz: entry.frequency_hz,
                    mode:         mode_to_serde(entry.mode),
                },
            );
        }
        out
    }
}

// --------------------------------------------------------------------
// S-meter math (frontend-agnostic)
// --------------------------------------------------------------------

/// Convert a dBm reading to its IARU S-unit number on HF.
/// 6 dB per S-unit between S1 and S9, then over-S9 in 10 dB steps
/// (S9+10, +20, +40, +60). Returns a fractional value so the needle
/// moves smoothly between integer units.
pub fn dbm_to_s_units(dbm: f32) -> f32 {
    if dbm <= -73.0 {
        ((dbm + 127.0) / 6.0).clamp(0.0, 9.0)
    } else {
        9.0 + (dbm + 73.0) / 10.0
    }
}

// --------------------------------------------------------------------
// AppOptions: how the frontend wants the App to start
// --------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct AppOptions {
    /// Override the persisted radio IP. Used by the `HL2_IP=…` env var
    /// path so a one-liner debug session keeps working without erasing
    /// the user's saved IP.
    pub radio_ip_override: Option<String>,
}

// --------------------------------------------------------------------
// App: the view-model itself
// --------------------------------------------------------------------

/// View-model owned by every frontend. Holds all the persisted UI
/// state, the optional live `Radio` handle, and the dirty/save
/// bookkeeping. Frontends call read methods to render and write
/// methods to dispatch user actions.
pub struct App {
    // --- Live radio handle (None = disconnected) --------------------
    radio:      Option<Radio>,
    telemetry:  Option<Arc<ArcSwap<Telemetry>>>,
    last_error: Option<String>,

    // --- UI state / form fields ------------------------------------
    radio_ip: String,
    audio_device: String,
    /// How many receivers to request on the next `Connect`. Fixed
    /// for the lifetime of a session; changing it requires a
    /// disconnect/reconnect cycle.
    num_rx:    u8,
    rxs:       Vec<RxState>,
    /// Index of the RX that band-button presses + scripts target.
    active_rx: usize,
    band_stack: BandStack,

    // --- Settings sections (D.10) ------------------------------------
    display_settings: arion_settings::DisplaySettings,
    dsp_defaults:     arion_settings::DspDefaults,
    calibration:      arion_settings::Calibration,
    network_settings: arion_settings::NetworkSettings,
    midi_settings:    arion_settings::MidiSettings,

    // --- Persistence (B.5) -----------------------------------------
    memories:  Vec<Memory>,
    /// Window visibility flags, keyed by [`WindowKind`]. Frontends
    /// honor these — egui shows/hides floating windows, TUI shows/
    /// hides bottom panes.
    open_windows: std::collections::HashMap<WindowKind, bool>,
    last_save:    Instant,
    dirty:        bool,
}

impl App {
    /// Build a new `App`, loading persisted settings from the default
    /// platform config dir. Failures during load are non-fatal — the
    /// app falls back to defaults so a corrupted arion.toml never
    /// bricks the user.
    pub fn new(opts: AppOptions) -> Self {
        let settings = match Settings::load_default() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load settings, using defaults");
                let mut s = Settings::default();
                s.ensure_rx_slots(2);
                s
            }
        };

        let radio_ip = opts
            .radio_ip_override
            .unwrap_or_else(|| settings.general.last_radio_ip.clone());

        let mut rxs: Vec<RxState> = Vec::with_capacity(MAX_RX);
        for r in 0..MAX_RX {
            let serde_rx = settings.rxs.get(r).cloned().unwrap_or_default();
            let mode = mode_from_serde(serde_rx.mode);
            let (flo, fhi) = mode.default_passband_hz();
            rxs.push(RxState {
                enabled:      serde_rx.enabled || r == 0,
                frequency_hz: serde_rx.frequency_hz,
                mode,
                volume:       serde_rx.volume,
                nr3:          serde_rx.nr3,
                nr4:          serde_rx.nr4,
                anr:          serde_rx.anr,
                emnr:         serde_rx.emnr,
                squelch:         serde_rx.squelch,
                squelch_db:      serde_rx.squelch_db,
                apf:             serde_rx.apf,
                apf_freq_hz:     serde_rx.apf_freq_hz,
                apf_bw_hz:       serde_rx.apf_bw_hz,
                apf_gain_db:     serde_rx.apf_gain_db,
                agc_top_dbm:     serde_rx.agc_top_dbm,
                agc_hang_level:  serde_rx.agc_hang_level,
                agc_decay_ms:    serde_rx.agc_decay_ms,
                agc_fixed_gain:  serde_rx.agc_fixed_gain,
                fm_deviation_hz: serde_rx.fm_deviation_hz,
                ctcss_on:        serde_rx.ctcss_on,
                ctcss_hz:        serde_rx.ctcss_hz,
                filter_lo:    flo,
                filter_hi:    fhi,
                ..RxState::default()
            });
        }

        let band_stack = BandStack::from_settings(&settings.band_stacks);

        App {
            radio:        None,
            telemetry:    None,
            last_error:   None,
            radio_ip,
            audio_device: settings.general.audio_device.clone(),
            num_rx:       settings.general.num_rx.clamp(1, MAX_RX as u8),
            rxs,
            active_rx:    settings.general.active_rx.clamp(0, MAX_RX as u8 - 1) as usize,
            band_stack,
            display_settings: settings.display,
            dsp_defaults:     settings.dsp,
            calibration:      settings.calibration,
            network_settings: settings.network,
            midi_settings:    settings.midi,
            memories:     settings.memories,
            open_windows: std::collections::HashMap::new(),
            last_save:    Instant::now(),
            dirty:        false,
        }
    }

    // --- Read API (immuable, called by frontends to render) ---------

    pub fn radio_ip(&self) -> &str { &self.radio_ip }
    pub fn num_rx(&self) -> u8 { self.num_rx }
    pub fn rxs(&self) -> &[RxState] { &self.rxs }
    pub fn rx(&self, rx: usize) -> Option<&RxState> { self.rxs.get(rx) }
    pub fn active_rx(&self) -> usize { self.active_rx }
    pub fn band_stack(&self) -> &BandStack { &self.band_stack }
    pub fn memories(&self) -> &[Memory] { &self.memories }
    pub fn last_error(&self) -> Option<&str> { self.last_error.as_deref() }
    pub fn is_connected(&self) -> bool { self.radio.is_some() }
    pub fn radio(&self) -> Option<&Radio> { self.radio.as_ref() }
    pub fn telemetry(&self) -> Option<&Arc<ArcSwap<Telemetry>>> { self.telemetry.as_ref() }
    pub fn telemetry_snapshot(&self) -> Option<Arc<Telemetry>> {
        self.telemetry.as_ref().map(|t| t.load_full())
    }

    /// Whether the named floating window should be shown.
    pub fn window_open(&self, w: WindowKind) -> bool {
        self.open_windows.get(&w).copied().unwrap_or(false)
    }

    pub fn settings_path(&self) -> Option<PathBuf> {
        Settings::default_path()
    }

    pub fn audio_device_name(&self) -> &str {
        &self.audio_device
    }

    pub fn set_audio_device_name(&mut self, name: String) {
        self.audio_device = name;
        self.mark_dirty();
    }

    pub fn display_settings(&self) -> &arion_settings::DisplaySettings {
        &self.display_settings
    }
    pub fn display_settings_mut(&mut self) -> &mut arion_settings::DisplaySettings {
        self.mark_dirty();
        &mut self.display_settings
    }
    pub fn dsp_defaults(&self) -> &arion_settings::DspDefaults {
        &self.dsp_defaults
    }
    pub fn dsp_defaults_mut(&mut self) -> &mut arion_settings::DspDefaults {
        self.mark_dirty();
        &mut self.dsp_defaults
    }
    pub fn calibration(&self) -> &arion_settings::Calibration {
        &self.calibration
    }
    pub fn calibration_mut(&mut self) -> &mut arion_settings::Calibration {
        self.mark_dirty();
        &mut self.calibration
    }
    pub fn network_settings(&self) -> &arion_settings::NetworkSettings {
        &self.network_settings
    }
    pub fn network_settings_mut(&mut self) -> &mut arion_settings::NetworkSettings {
        self.mark_dirty();
        &mut self.network_settings
    }
    pub fn midi_settings(&self) -> &arion_settings::MidiSettings {
        &self.midi_settings
    }
    pub fn midi_settings_mut(&mut self) -> &mut arion_settings::MidiSettings {
        self.mark_dirty();
        &mut self.midi_settings
    }

    // --- Write API (mut self, dispatched from frontends) ------------

    /// Edit the radio IP from a text input. Marks dirty if changed.
    pub fn set_radio_ip(&mut self, ip: String) {
        if self.radio_ip != ip {
            self.radio_ip = ip;
            self.mark_dirty();
        }
    }

    /// Set the number of receivers requested on the next Connect.
    /// Only effective while disconnected.
    pub fn set_num_rx(&mut self, num: u8) {
        let n = num.clamp(1, MAX_RX as u8);
        if self.num_rx != n && self.radio.is_none() {
            self.num_rx = n;
            self.mark_dirty();
        }
    }

    pub fn set_rx_enabled(&mut self, rx: u8, enabled: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.enabled == enabled {
            return;
        }
        view.enabled = enabled;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_enabled(rx, enabled);
        }
        self.mark_dirty();
    }

    pub fn set_rx_frequency(&mut self, rx: u8, hz: u32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.locked || view.frequency_hz == hz {
            return;
        }
        view.frequency_hz = hz;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_frequency(rx, hz);
        }
        self.mark_dirty();
    }

    pub fn set_rx_mode(&mut self, rx: u8, mode: WdspMode) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.mode == mode {
            return;
        }
        view.mode = mode;
        // Reset filter to the mode's default passband on mode change,
        // matching Arion upstream behaviour.
        let (lo, hi) = mode.default_passband_hz();
        view.filter_lo = lo;
        view.filter_hi = hi;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_mode(rx, mode);
            let _ = r.set_rx_passband(rx, lo, hi);
        }
        self.mark_dirty();
    }

    pub fn set_rx_volume(&mut self, rx: u8, volume: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if (view.volume - volume).abs() < f32::EPSILON {
            return;
        }
        view.volume = volume;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_volume(rx, volume);
        }
        self.mark_dirty();
    }

    pub fn set_rx_nr3(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.nr3 == on {
            return;
        }
        view.nr3 = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_nr3(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_nr4(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.nr4 == on {
            return;
        }
        view.nr4 = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_nr4(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_anr(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.anr == on {
            return;
        }
        view.anr = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_anr(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_emnr(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if view.emnr == on {
            return;
        }
        view.emnr = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_emnr(rx, on);
        }
        self.mark_dirty();
    }

    // --- E.10 Squelch ---

    pub fn set_rx_squelch(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.squelch = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_squelch_run(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_squelch_threshold(&mut self, rx: u8, db: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.squelch_db = db;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_squelch_threshold(rx, db as f64);
        }
        self.mark_dirty();
    }

    // --- E.11 APF ---

    pub fn set_rx_apf(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.apf = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_apf_run(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_apf_freq(&mut self, rx: u8, hz: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.apf_freq_hz = hz;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_apf_freq(rx, hz as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_apf_bandwidth(&mut self, rx: u8, hz: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.apf_bw_hz = hz;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_apf_bandwidth(rx, hz as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_apf_gain(&mut self, rx: u8, db: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.apf_gain_db = db;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_apf_gain(rx, db as f64);
        }
        self.mark_dirty();
    }

    // --- E.12 AGC fine ---

    pub fn set_rx_agc_top(&mut self, rx: u8, dbm: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.agc_top_dbm = dbm;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_agc_top(rx, dbm as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_agc_hang_level(&mut self, rx: u8, level: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.agc_hang_level = level;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_agc_hang_level(rx, level as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_agc_decay(&mut self, rx: u8, ms: i32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.agc_decay_ms = ms;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_agc_decay(rx, ms);
        }
        self.mark_dirty();
    }

    pub fn set_rx_agc_fixed_gain(&mut self, rx: u8, db: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.agc_fixed_gain = db;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_agc_fixed_gain(rx, db as f64);
        }
        self.mark_dirty();
    }

    // --- E.13 FM ---

    pub fn set_rx_fm_deviation(&mut self, rx: u8, hz: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.fm_deviation_hz = hz;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_fm_deviation(rx, hz as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_ctcss(&mut self, rx: u8, on: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.ctcss_on = on;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_ctcss_run(rx, on);
        }
        self.mark_dirty();
    }

    pub fn set_rx_ctcss_freq(&mut self, rx: u8, hz: f32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.ctcss_hz = hz;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_ctcss_freq(rx, hz as f64);
        }
        self.mark_dirty();
    }

    pub fn set_rx_agc(&mut self, rx: u8, agc: AgcPreset) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.agc_mode = agc;
        self.mark_dirty();
    }

    /// Set the Receiver Incremental Tuning offset in Hz. Display-only
    /// today; the WDSP wiring will be added once the TX path lands.
    /// Clamped to ±10 kHz which matches typical transceiver ranges.
    pub fn set_rx_rit(&mut self, rx: u8, hz: i32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.rit_hz = hz.clamp(-10_000, 10_000);
        self.mark_dirty();
    }

    pub fn set_rx_muted(&mut self, rx: u8, muted: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.muted = muted;
        self.mark_dirty();
    }

    pub fn set_rx_locked(&mut self, rx: u8, locked: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.locked = locked;
        self.mark_dirty();
    }

    pub fn toggle_rx_flag(&mut self, rx: u8, flag: &str) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        let new_val;
        match flag {
            "nb"   => { view.nb   = !view.nb;   new_val = view.nb; }
            "nb2"  => { view.nb2  = !view.nb2;  new_val = view.nb2; }
            "anf"  => { view.anf  = !view.anf;  new_val = view.anf; }
            "bin"  => { view.bin  = !view.bin;  new_val = view.bin; }
            "tnf"  => { view.tnf  = !view.tnf;  new_val = view.tnf; }
            "anr"  => { view.anr  = !view.anr;  new_val = view.anr; }
            "emnr" => { view.emnr = !view.emnr; new_val = view.emnr; }
            _ => return,
        }
        // Push to live radio DSP where bindings exist
        if let Some(r) = &self.radio {
            match flag {
                "anf"  => { let _ = r.set_rx_anf(rx, new_val); }
                "bin"  => { let _ = r.set_rx_binaural(rx, new_val); }
                "anr"  => { let _ = r.set_rx_anr(rx, new_val); }
                "emnr" => { let _ = r.set_rx_emnr(rx, new_val); }
                // NB/NB2/TNF: upstream WDSP uses low-level ANB/NOB
                // structures, not simple SetRXA* calls. Binding
                // deferred until the full NB pipeline is understood.
                _ => {}
            }
        }
        self.mark_dirty();
    }

    pub fn set_rx_eq_enabled(&mut self, rx: u8, enabled: bool) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.eq_enabled = enabled;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_eq_run(rx, enabled);
        }
        self.mark_dirty();
    }

    pub fn set_rx_eq_gains(&mut self, rx: u8, gains: [i32; 11]) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.eq_gains = gains;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_eq_bands(rx, gains);
        }
        self.mark_dirty();
    }

    pub fn set_rx_eq_band(&mut self, rx: u8, band_idx: usize, gain_db: i32) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        if band_idx < 11 {
            view.eq_gains[band_idx] = gain_db;
            let gains = view.eq_gains;
            if let Some(r) = &self.radio {
                let _ = r.set_rx_eq_bands(rx, gains);
            }
            self.mark_dirty();
        }
    }

    /// Set the passband directly (variable filter).
    pub fn set_rx_filter(&mut self, rx: u8, lo: f64, hi: f64) {
        let Some(view) = self.rxs.get_mut(rx as usize) else { return };
        view.filter_lo = lo;
        view.filter_hi = hi;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_passband(rx, lo, hi);
        }
        self.mark_dirty();
    }

    /// Apply a named filter preset, computing (lo, hi) from the
    /// preset width and the active mode.
    pub fn set_rx_filter_preset(&mut self, rx: u8, preset: FilterPreset) {
        let Some(view) = self.rxs.get(rx as usize) else { return };
        let (lo, hi) = preset.passband_for_mode(view.mode);
        self.set_rx_filter(rx, lo, hi);
    }

    /// Promote `rx` to the active RX (target of band buttons,
    /// scripted commands, keyboard shortcuts).
    pub fn set_active_rx(&mut self, rx: usize) {
        if rx < self.rxs.len() && self.active_rx != rx {
            self.active_rx = rx;
            self.mark_dirty();
        }
    }

    /// Apply the stored entry for `band` to the active RX. Saves the
    /// active RX's current freq/mode back to its current band first
    /// so jump-away → jump-back preserves where you were.
    pub fn jump_to_band(&mut self, band: Band) {
        let rx = self.active_rx;
        if rx >= self.rxs.len() {
            return;
        }

        let current_freq = self.rxs[rx].frequency_hz;
        let current_mode = self.rxs[rx].mode;
        if let Some(prev_band) = Band::for_freq(current_freq) {
            self.band_stack.set(prev_band, BandStackEntry {
                frequency_hz: current_freq,
                mode:         current_mode,
            });
        }

        let entry = self.band_stack.get(band);
        self.rxs[rx].frequency_hz = entry.frequency_hz;
        self.rxs[rx].mode         = entry.mode;

        if let Some(r) = &self.radio {
            let _ = r.set_rx_frequency(rx as u8, entry.frequency_hz);
            let _ = r.set_rx_mode(rx as u8, entry.mode);
        }
        self.mark_dirty();
    }

    // --- Memories ---------------------------------------------------

    pub fn add_memory(&mut self, memory: Memory) {
        self.memories.push(memory);
        self.mark_dirty();
    }

    pub fn delete_memory(&mut self, idx: usize) {
        if idx < self.memories.len() {
            self.memories.remove(idx);
            self.mark_dirty();
        }
    }

    /// Apply memory at index `idx` to the active RX (frequency + mode).
    pub fn load_memory(&mut self, idx: usize) {
        let Some(mem) = self.memories.get(idx).cloned() else { return };
        let rx = self.active_rx;
        if rx >= self.rxs.len() {
            return;
        }
        let mode = mode_from_serde(mem.mode);
        self.rxs[rx].frequency_hz = mem.freq_hz;
        self.rxs[rx].mode         = mode;
        if let Some(r) = &self.radio {
            let _ = r.set_rx_frequency(rx as u8, mem.freq_hz);
            let _ = r.set_rx_mode(rx as u8, mode);
        }
        self.mark_dirty();
    }

    // --- Window toggles --------------------------------------------

    pub fn toggle_window(&mut self, w: WindowKind) {
        let entry = self.open_windows.entry(w).or_insert(false);
        *entry = !*entry;
    }

    pub fn set_window_open(&mut self, w: WindowKind, open: bool) {
        self.open_windows.insert(w, open);
    }

    // --- Connect / disconnect --------------------------------------

    /// Try to connect to the radio at `radio_ip`. Stores any error
    /// in `last_error` so frontends can render it inline.
    pub fn connect(&mut self) {
        let addr_str = format!("{}:1024", self.radio_ip);
        let addr = match addr_str.parse() {
            Ok(a) => a,
            Err(e) => {
                self.last_error = Some(format!("invalid IP: {e}"));
                return;
            }
        };

        let audio_dev = if self.audio_device.is_empty() {
            None
        } else {
            Some(self.audio_device.clone())
        };
        let mut config = RadioConfig {
            radio_addr:    addr,
            num_rx:        self.num_rx,
            audio_device:  audio_dev,
            prime_wisdom:  true,
            ..RadioConfig::default()
        };
        for (r, view) in self.rxs.iter().enumerate().take(self.num_rx as usize) {
            config.rx[r] = RxConfig {
                enabled:      view.enabled,
                frequency_hz: view.frequency_hz,
                mode:         view.mode,
                volume:       view.volume,
            };
        }

        match Radio::start(config) {
            Ok(r) => {
                self.telemetry  = Some(r.telemetry());
                self.radio      = Some(r);
                self.last_error = None;
                // Connect snapshots the form fields the user just
                // committed so a crash mid-session keeps the most
                // recent intent on disk.
                self.save_now();
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
            }
        }
    }

    /// Stop the live radio (if any) and force-save.
    pub fn disconnect(&mut self) {
        if let Some(r) = self.radio.take() {
            let _ = r.stop();
        }
        self.telemetry = None;
        self.save_now();
    }

    // --- Lifecycle / persistence ------------------------------------

    /// Once-per-frame tick from the frontend. Currently does only the
    /// debounced auto-save; phase D.12 will also drain the script
    /// scheduler queue and the event bus from here.
    pub fn tick(&mut self, _now: Instant) {
        self.maybe_autosave();
    }

    /// Called by the frontend when the application is about to exit.
    /// Cleanly disconnects the radio and forces a final save.
    pub fn shutdown(&mut self) {
        if self.radio.is_some() {
            self.disconnect();
        } else {
            self.save_now();
        }
    }

    /// Mark the in-memory settings dirty so the next debounce tick
    /// (or the next disconnect / quit) writes them to disk.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Persist the current state to disk **right now**, regardless
    /// of the debounce.
    pub fn save_now(&mut self) {
        match self.to_settings().save_default() {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
                tracing::debug!("settings saved");
            }
            Err(e) => tracing::warn!(error = %e, "settings save failed"),
        }
    }

    fn maybe_autosave(&mut self) {
        if self.dirty && self.last_save.elapsed() >= SAVE_DEBOUNCE {
            self.save_now();
        }
    }

    /// Build a `Settings` snapshot from the current view-model state.
    fn to_settings(&self) -> Settings {
        let mut s = Settings::default();
        s.ensure_rx_slots(MAX_RX);
        s.general = GeneralSettings {
            last_radio_ip: self.radio_ip.clone(),
            audio_device:  self.audio_device.clone(),
            active_rx:     self.active_rx as u8,
            num_rx:        self.num_rx,
        };
        for (i, view) in self.rxs.iter().enumerate().take(MAX_RX) {
            s.rxs[i] = SerdeRxSettings {
                enabled:      view.enabled,
                frequency_hz: view.frequency_hz,
                mode:         mode_to_serde(view.mode),
                volume:       view.volume,
                nr3:          view.nr3,
                nr4:          view.nr4,
                anr:          view.anr,
                emnr:         view.emnr,
                squelch:         view.squelch,
                squelch_db:      view.squelch_db,
                apf:             view.apf,
                apf_freq_hz:     view.apf_freq_hz,
                apf_bw_hz:       view.apf_bw_hz,
                apf_gain_db:     view.apf_gain_db,
                agc_top_dbm:     view.agc_top_dbm,
                agc_hang_level:  view.agc_hang_level,
                agc_decay_ms:    view.agc_decay_ms,
                agc_fixed_gain:  view.agc_fixed_gain,
                fm_deviation_hz: view.fm_deviation_hz,
                ctcss_on:        view.ctcss_on,
                ctcss_hz:        view.ctcss_hz,
            };
        }
        s.band_stacks  = self.band_stack.to_settings();
        s.display      = self.display_settings.clone();
        s.dsp          = self.dsp_defaults.clone();
        s.calibration  = self.calibration.clone();
        s.network      = self.network_settings.clone();
        s.midi         = self.midi_settings.clone();
        s.memories     = self.memories.clone();
        s
    }
}

// --------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbm_to_s_units_known_points() {
        // S1 = -127 dBm, S9 = -73 dBm, S9+10 = -63 dBm.
        assert!((dbm_to_s_units(-127.0) - 0.0).abs() < 0.01);
        assert!((dbm_to_s_units( -73.0) - 9.0).abs() < 0.01);
        assert!((dbm_to_s_units( -63.0) - 10.0).abs() < 0.01);
        assert!((dbm_to_s_units( -53.0) - 11.0).abs() < 0.01);
    }

    #[test]
    fn band_for_freq_round_trip() {
        for b in Band::ALL {
            let entry = b.default_entry();
            assert_eq!(Band::for_freq(entry.frequency_hz), Some(b));
        }
    }

    #[test]
    fn band_stack_default_seeded() {
        let bs = BandStack::default();
        // 40m default should be the FT8 anchor.
        assert_eq!(bs.get(Band::M40).frequency_hz, 7_074_000);
        assert_eq!(bs.get(Band::M40).mode, WdspMode::Usb);
    }

    #[test]
    fn band_stack_settings_round_trip() {
        let bs = BandStack::default();
        let map = bs.to_settings();
        let bs2 = BandStack::from_settings(&map);
        for b in Band::ALL {
            assert_eq!(bs.get(b).frequency_hz, bs2.get(b).frequency_hz);
            assert_eq!(bs.get(b).mode,         bs2.get(b).mode);
        }
    }

    #[test]
    fn mode_adapter_round_trip() {
        for m in [
            WdspMode::Lsb, WdspMode::Usb, WdspMode::Dsb, WdspMode::CwL,
            WdspMode::CwU, WdspMode::Fm, WdspMode::Am, WdspMode::DigU,
            WdspMode::Spec, WdspMode::DigL, WdspMode::Sam, WdspMode::Drm,
        ] {
            assert_eq!(mode_from_serde(mode_to_serde(m)), m);
        }
    }
}
