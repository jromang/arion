//! Persistent user settings for Arion, serialised as TOML.
//!
//! This crate is the bottom layer of the persistence stack — it owns
//! the serializable types and the load/save plumbing, and stays free
//! of any dependency on `wdsp` / `arion-core` / `arion-egui`. The UI
//! layer converts to/from these types at the boundary so the on-disk
//! schema doesn't bleed into runtime structs and vice-versa.
//!
//! On-disk layout (`arion.toml` under `$XDG_CONFIG_HOME/arion/`):
//!
//! ```toml
//! [general]
//! last_radio_ip = "192.168.1.40"
//! audio_device  = ""
//! active_rx     = 0
//! num_rx        = 1
//!
//! [[rxs]]
//! enabled       = true
//! frequency_hz  = 7074000
//! mode          = "Usb"
//! volume        = 0.25
//! nr3           = false
//! nr4           = false
//!
//! [band_stacks.40]
//! frequency_hz = 7074000
//! mode         = "Usb"
//!
//! [[memories]]
//! name    = "WWV 10 MHz"
//! freq_hz = 10000000
//! mode    = "Am"
//! tag     = "Time signal"
//! ```
//!
//! Loading is forgiving: a missing file produces `Settings::default()`,
//! and unknown fields are tolerated by `serde(default)` so a future
//! schema bump doesn't lock out an old binary. Saving is atomic via
//! a sibling tempfile + `rename` so a SIGKILL during the write can't
//! leave the user with a half-written `arion.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Errors returned by [`Settings::load`] / [`Settings::save`]. Most
/// callers can downgrade these to a `tracing::warn` and fall back to
/// `Settings::default()` — none of them are fatal at the radio layer.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not determine the config directory for this platform")]
    NoConfigDir,
    #[error("failed to create config directory {path:?}: {source}")]
    CreateDir { path: PathBuf, #[source] source: std::io::Error },
    #[error("failed to read settings file {path:?}: {source}")]
    Read { path: PathBuf, #[source] source: std::io::Error },
    #[error("failed to write settings file {path:?}: {source}")]
    Write { path: PathBuf, #[source] source: std::io::Error },
    #[error("failed to parse settings file {path:?}: {source}")]
    ParseToml { path: PathBuf, #[source] source: toml::de::Error },
    #[error("failed to serialise settings: {0}")]
    EncodeToml(#[from] toml::ser::Error),
}

// --- Mode mirror --------------------------------------------------------

/// String-serialised mirror of `wdsp::Mode`. Lives here so the
/// `arion-settings` crate doesn't need to depend on `wdsp` (which
/// would pull the entire DSP / FFI tree into the persistence layer).
/// Variants and their string forms match `wdsp::Mode` 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    Lsb,
    #[default]
    Usb,
    Dsb,
    CwL,
    CwU,
    Fm,
    Am,
    DigU,
    Spec,
    DigL,
    Sam,
    Drm,
}

// --- Display / DSP / Calibration settings --------------------------------

/// Waterfall colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WaterfallPalette {
    /// 8-stop gradient matching Thetis "Enhanced" (default).
    #[default]
    Enhanced,
    /// Blue → magenta → red → orange (previous Arion default, "Classic").
    Classic,
    /// Black → white.
    Greyscale,
    /// Black → purple → orange → yellow → white.
    Thermal,
    /// Black → bright green (like Spectran).
    Spectran,
}

impl WaterfallPalette {
    pub const ALL: &'static [WaterfallPalette] = &[
        WaterfallPalette::Enhanced,
        WaterfallPalette::Classic,
        WaterfallPalette::Greyscale,
        WaterfallPalette::Thermal,
        WaterfallPalette::Spectran,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WaterfallPalette::Enhanced  => "Enhanced",
            WaterfallPalette::Classic   => "Classic",
            WaterfallPalette::Greyscale => "Greyscale",
            WaterfallPalette::Thermal   => "Thermal",
            WaterfallPalette::Spectran  => "Spectran",
        }
    }
}

/// IARU region the bandplan overlay is drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BandplanRegion {
    #[default]
    Region1, // Europe, Africa, Middle East, Northern Asia
    Region2, // Americas
    Region3, // Asia-Pacific, Oceania
    Off,
}

impl BandplanRegion {
    pub const ALL: &'static [BandplanRegion] = &[
        BandplanRegion::Off,
        BandplanRegion::Region1,
        BandplanRegion::Region2,
        BandplanRegion::Region3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BandplanRegion::Off     => "Off",
            BandplanRegion::Region1 => "Region 1 (EU/AF)",
            BandplanRegion::Region2 => "Region 2 (AM)",
            BandplanRegion::Region3 => "Region 3 (AP)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    pub spectrum_min_db:    f32,
    pub spectrum_max_db:    f32,
    pub waterfall_speed:    u8,
    pub waterfall_palette:  WaterfallPalette,
    pub bandplan_region:    BandplanRegion,
    pub auto_connect:       bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        DisplaySettings {
            spectrum_min_db:   -120.0,
            spectrum_max_db:      0.0,
            waterfall_speed:      1,
            waterfall_palette:    WaterfallPalette::Enhanced,
            bandplan_region:      BandplanRegion::Region1,
            auto_connect:         false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DspDefaults {
    pub agc_mode:       String,
    pub nr3_default:    bool,
    pub nr4_default:    bool,
    pub nr4_reduction:  f32,
}

impl Default for DspDefaults {
    fn default() -> Self {
        DspDefaults {
            agc_mode:      "Med".into(),
            nr3_default:   false,
            nr4_default:   false,
            nr4_reduction: 10.0,
        }
    }
}

/// Network-facing services (currently only the rigctld server).
/// Defaults are "off" so a fresh install doesn't surprise the user
/// with an open TCP port.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    pub rigctld_enabled:   bool,
    pub rigctld_port:      u16,
    pub api_enabled:       bool,
    pub api_port:          u16,
    pub api_bind_loopback: bool,
    pub api_allow_scripts: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        NetworkSettings {
            rigctld_enabled:   false,
            rigctld_port:      4532,
            api_enabled:       false,
            api_port:          8081,
            api_bind_loopback: true,
            api_allow_scripts: false,
        }
    }
}

/// MIDI controller integration. The binding table itself lives in
/// a separate file (`~/.config/arion/midi.toml`) because it's owned
/// by `arion-midi` and those types can't be referenced from here
/// without creating a dependency cycle with `arion-app`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiSettings {
    pub enabled:     bool,
    pub device_name: Option<String>,
}

/// Per-band S-meter calibration offset in dBm. Stored as a map
/// keyed by band label ("160", "80", …). Missing entries → 0.0 dBm
/// offset (no correction).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub smeter_offsets: BTreeMap<String, f32>,
}

// --- Top-level Settings -------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub general:     GeneralSettings,
    pub display:     DisplaySettings,
    pub dsp:         DspDefaults,
    pub calibration: Calibration,
    pub network:     NetworkSettings,
    pub midi:        MidiSettings,
    /// Per-RX state, ordered by RX index. Always at least 2 entries
    /// after a load — `Settings::ensure_rx_slots` pads with defaults
    /// so the UI can index without bounds-checking.
    pub rxs:         Vec<RxSettings>,
    /// Band stack keyed by short band label ("160", "80", "40", …).
    pub band_stacks: BTreeMap<String, BandStackEntry>,
    pub memories:    Vec<Memory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    pub last_radio_ip: String,
    pub audio_device:  String,
    /// Index of the RX whose VFO + mode the band-buttons / keyboard
    /// shortcuts target. 0 = RX1, 1 = RX2, etc.
    pub active_rx:     u8,
    /// Number of receivers to request on the next Connect.
    pub num_rx:        u8,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings {
            last_radio_ip: "192.168.1.40".into(),
            audio_device:  String::new(),
            active_rx:     0,
            num_rx:        1,
        }
    }
}

/// One tracking-notch filter entry. Persisted per-RX.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TnfNotch {
    pub freq_hz:  f64,
    pub width_hz: f64,
    pub active:   bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RxSettings {
    pub enabled:      bool,
    pub frequency_hz: u32,
    pub mode:         Mode,
    pub volume:       f32,
    pub nr3:          bool,
    pub nr4:          bool,
    #[serde(default)]
    pub anr:          bool,
    #[serde(default)]
    pub emnr:         bool,
    #[serde(default)]
    pub squelch:         bool,
    #[serde(default = "default_squelch_db")]
    pub squelch_db:      f32,
    #[serde(default)]
    pub apf:             bool,
    #[serde(default = "default_apf_freq")]
    pub apf_freq_hz:     f32,
    #[serde(default = "default_apf_bw")]
    pub apf_bw_hz:       f32,
    #[serde(default = "default_apf_gain")]
    pub apf_gain_db:     f32,
    #[serde(default = "default_agc_top")]
    pub agc_top_dbm:     f32,
    #[serde(default = "default_agc_hang_level")]
    pub agc_hang_level:  f32,
    #[serde(default = "default_agc_decay")]
    pub agc_decay_ms:    i32,
    #[serde(default = "default_agc_fixed_gain")]
    pub agc_fixed_gain:  f32,
    #[serde(default = "default_fm_deviation")]
    pub fm_deviation_hz: f32,
    #[serde(default)]
    pub ctcss_on:        bool,
    #[serde(default = "default_ctcss_hz")]
    pub ctcss_hz:        f32,
    #[serde(default)]
    pub tnf_notches:     Vec<TnfNotch>,
    #[serde(default)]
    pub sam_submode:     u8,
}

fn default_squelch_db()     -> f32 { -30.0 }
fn default_apf_freq()       -> f32 { 600.0 }
fn default_apf_bw()         -> f32 { 50.0 }
fn default_apf_gain()       -> f32 { 6.0 }
fn default_agc_top()        -> f32 { -30.0 }
fn default_agc_hang_level() -> f32 { -20.0 }
fn default_agc_decay()      -> i32 { 250 }
fn default_agc_fixed_gain() -> f32 { 10.0 }
fn default_fm_deviation()   -> f32 { 5000.0 }
fn default_ctcss_hz()       -> f32 { 67.0 }

impl Default for RxSettings {
    fn default() -> Self {
        RxSettings {
            enabled:      false,
            frequency_hz: 7_074_000,
            mode:         Mode::Usb,
            volume:       0.25,
            nr3:          false,
            nr4:          false,
            anr:          false,
            emnr:         false,
            squelch:         false,
            squelch_db:      default_squelch_db(),
            apf:             false,
            apf_freq_hz:     default_apf_freq(),
            apf_bw_hz:       default_apf_bw(),
            apf_gain_db:     default_apf_gain(),
            agc_top_dbm:     default_agc_top(),
            agc_hang_level:  default_agc_hang_level(),
            agc_decay_ms:    default_agc_decay(),
            agc_fixed_gain:  default_agc_fixed_gain(),
            fm_deviation_hz: default_fm_deviation(),
            ctcss_on:        false,
            ctcss_hz:        default_ctcss_hz(),
            tnf_notches:     Vec::new(),
            sam_submode:     0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct BandStackEntry {
    pub frequency_hz: u32,
    pub mode:         Mode,
}

impl Default for BandStackEntry {
    fn default() -> Self {
        BandStackEntry {
            frequency_hz: 7_074_000,
            mode:         Mode::Usb,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub name:    String,
    pub freq_hz: u32,
    pub mode:    Mode,
    pub tag:     String,
}

impl Default for Memory {
    fn default() -> Self {
        Memory {
            name:    String::new(),
            freq_hz: 0,
            mode:    Mode::Usb,
            tag:     String::new(),
        }
    }
}

// --- Load / save --------------------------------------------------------

impl Settings {
    /// Filename used inside the config dir. Exposed as a constant so
    /// tests can drop a fixture next to a tempdir.
    pub const FILENAME: &'static str = "arion.toml";

    /// Default config-file path: `$XDG_CONFIG_HOME/arion/arion.toml`
    /// on Linux, `~/Library/Application Support/arion/arion.toml`
    /// on macOS, `%APPDATA%\arion\arion.toml` on Windows.
    /// Returns `None` only on headless containers with no `HOME`.
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("rs", "arion", "arion")
            .map(|p| p.config_dir().join(Self::FILENAME))
    }

    /// Load settings from `path`. Missing file → `Settings::default()`
    /// (not an error: a fresh install starts blank). Parse errors are
    /// surfaced so the user/app can decide whether to overwrite the
    /// file or bail out.
    pub fn load_from(path: &Path) -> Result<Self, SettingsError> {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "no settings file, using defaults");
                let mut s = Settings::default();
                s.ensure_rx_slots(2);
                return Ok(s);
            }
            Err(e) => {
                return Err(SettingsError::Read {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let mut s: Settings = toml::from_str(&contents).map_err(|e| SettingsError::ParseToml {
            path: path.to_path_buf(),
            source: e,
        })?;
        s.ensure_rx_slots(2);
        Ok(s)
    }

    /// Convenience: load from the default platform path. Returns
    /// `Ok(Settings::default())` (not an error) when no config dir
    /// is available — the radio still has to start.
    pub fn load_default() -> Result<Self, SettingsError> {
        match Self::default_path() {
            Some(path) => Self::load_from(&path),
            None => {
                tracing::warn!("no config directory available, using default settings");
                let mut s = Settings::default();
                s.ensure_rx_slots(2);
                Ok(s)
            }
        }
    }

    /// Atomic save: write to `<path>.tmp` first, then `rename` over
    /// the target. The rename is atomic on every filesystem we care
    /// about (ext4, btrfs, APFS, NTFS via mingw stdlib), so a SIGKILL
    /// or power loss between the two calls leaves either the previous
    /// file or the new one — never a half-written file.
    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| SettingsError::CreateDir {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let toml_text = toml::to_string_pretty(self)?;

        let tmp_path = path.with_extension("toml.tmp");
        let mut tmp = fs::File::create(&tmp_path).map_err(|e| SettingsError::Write {
            path: tmp_path.clone(),
            source: e,
        })?;
        tmp.write_all(toml_text.as_bytes())
            .map_err(|e| SettingsError::Write { path: tmp_path.clone(), source: e })?;
        tmp.sync_all()
            .map_err(|e| SettingsError::Write { path: tmp_path.clone(), source: e })?;
        drop(tmp);

        fs::rename(&tmp_path, path).map_err(|e| SettingsError::Write {
            path: path.to_path_buf(),
            source: e,
        })?;
        tracing::debug!(path = %path.display(), "settings saved");
        Ok(())
    }

    pub fn save_default(&self) -> Result<(), SettingsError> {
        let path = Self::default_path().ok_or(SettingsError::NoConfigDir)?;
        self.save_to(&path)
    }

    /// Pad `self.rxs` to at least `min_slots` with default RxSettings.
    /// Used after `load_from` so the UI can always index `rxs[0]` /
    /// `rxs[1]` without bounds-checking, even on a fresh config.
    pub fn ensure_rx_slots(&mut self, min_slots: usize) {
        while self.rxs.len() < min_slots {
            self.rxs.push(RxSettings::default());
        }
    }
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_defaults() {
        let mut s = Settings::default();
        s.ensure_rx_slots(2);
        s.band_stacks.insert(
            "40".into(),
            BandStackEntry { frequency_hz: 7_074_000, mode: Mode::Usb },
        );
        s.memories.push(Memory {
            name:    "WWV".into(),
            freq_hz: 10_000_000,
            mode:    Mode::Am,
            tag:     "Time signal".into(),
        });

        let toml_text = toml::to_string_pretty(&s).unwrap();
        let parsed: Settings = toml::from_str(&toml_text).unwrap();
        assert_eq!(parsed.general.last_radio_ip, "192.168.1.40");
        assert_eq!(parsed.rxs.len(), 2);
        assert_eq!(parsed.band_stacks.get("40").unwrap().frequency_hz, 7_074_000);
        assert_eq!(parsed.memories[0].name, "WWV");
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempdir();
        let path = dir.join("arion.toml");
        let s = Settings::load_from(&path).unwrap();
        assert!(s.rxs.len() >= 2);
        // Cleanup happens via Drop on `dir`.
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = tempdir();
        let path = dir.join("arion.toml");

        let mut s = Settings::default();
        s.ensure_rx_slots(2);
        s.general.last_radio_ip = "10.0.0.1".into();
        s.rxs[0].frequency_hz = 14_074_000;
        s.rxs[0].mode = Mode::DigU;
        s.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.general.last_radio_ip, "10.0.0.1");
        assert_eq!(loaded.rxs[0].frequency_hz, 14_074_000);
        assert_eq!(loaded.rxs[0].mode, Mode::DigU);
    }

    #[test]
    fn forward_compat_unknown_field() {
        // A future schema with extra keys should still parse on an
        // older binary, thanks to serde(default) on every container.
        let toml_text = r#"
            [general]
            last_radio_ip = "1.2.3.4"
            future_field = "ignored"
        "#;
        let s: Settings = toml::from_str(toml_text).unwrap();
        assert_eq!(s.general.last_radio_ip, "1.2.3.4");
    }

    /// Tiny temp-dir helper without pulling in the `tempfile` crate
    /// (we want `arion-settings` to stay dep-light). Removes itself
    /// on drop. Not robust against panics in tests but good enough
    /// for the round-trip checks here.
    struct TestDir(PathBuf);
    impl TestDir {
        fn join(&self, n: &str) -> PathBuf { self.0.join(n) }
    }
    impl Drop for TestDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    fn tempdir() -> TestDir {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "arion-settings-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        p.push(unique);
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }
}
