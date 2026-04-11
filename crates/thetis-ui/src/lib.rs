//! egui + wgpu application shell for Thetis.
//!
//! Phase A layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │  [connect] [IP] │ VFO 7.074 MHz │ mode │ volume │ …      │  TopBar
//! ├──────────────────────────────────────────────────────────┤
//! │                                                          │
//! │                    spectrum (line)                       │
//! │                                                          │
//! ├──────────────────────────────────────────────────────────┤
//! │                                                          │
//! │                    waterfall (texture)                   │
//! │                                                          │
//! ├──────────────────────────────────────────────────────────┤
//! │  S-meter:  ▓▓▓▓▓░░░   -52 dBFS                           │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! The UI is 100% read-only against [`thetis_core::Telemetry`]; every
//! control action (connect, set-frequency, mode, volume) routes back
//! through a mutable [`Radio`] handle owned exclusively by the UI
//! thread. The DSP / network threads never see the `egui::Context`.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use thetis_core::{Radio, RadioConfig, RxConfig, Telemetry, WdspMode, MAX_RX, SPECTRUM_BINS};
use thetis_settings::{
    BandStackEntry as SerdeBandStackEntry, GeneralSettings, Memory, Mode as SerdeMode,
    RxSettings as SerdeRxSettings, Settings,
};

/// One-stop entry point for the binary: create and run the app.
///
/// Forces the wgpu renderer explicitly (Vulkan on Linux, Metal on
/// macOS, DX12 on Windows) so the build is truly cross-platform.
/// glow is not compiled in at all, per the workspace `eframe` feature
/// config.
pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 600.0])
            .with_title("Thetis-rs"),
        ..Default::default()
    };

    eframe::run_native(
        "Thetis-rs",
        options,
        Box::new(|cc| Ok(Box::new(ThetisApp::new(cc)))),
    )
}

/// Per-RX UI state: form-field values plus the RX's waterfall texture
/// cache. Stored in a fixed-size array inside [`ThetisApp`] so
/// disconnecting and reconnecting with a different `num_rx` doesn't
/// lose the user's last-edited frequency / mode.
struct RxView {
    enabled:      bool,
    frequency_hz: u32,
    mode:         WdspMode,
    volume:       f32,
    nr3:          bool,
    nr4:          bool,
    waterfall:    Waterfall,
}

/// Top-level eframe app state.
pub struct ThetisApp {
    // --- Live radio handle (None = disconnected) --------------------
    radio:     Option<Radio>,
    telemetry: Option<Arc<ArcSwap<Telemetry>>>,
    last_error: Option<String>,

    // --- UI state / form fields ------------------------------------
    radio_ip: String,
    /// How many receivers to request on the next `Connect`. Fixed
    /// for the lifetime of a session; changing it requires a
    /// disconnect/reconnect cycle.
    num_rx:   u8,
    rxs:      Vec<RxView>,
    /// Index of the RX that band-button presses affect. The user
    /// implicitly switches it by clicking inside that RX's spectrum
    /// or VFO controls. Defaults to RX0 on startup.
    active_rx: usize,
    /// Per-band cache of `(freq, mode)` so jumping away from 40 m
    /// and back returns to the user's last spot on that band.
    band_stack: BandStack,

    // --- Persistence (B.5) -----------------------------------------
    /// Memories list (named freq/mode bookmarks). Mirror of
    /// `Settings::memories` so the UI can mutate freely without
    /// touching disk on every keystroke.
    memories: Vec<Memory>,
    /// Whether the floating memories panel is visible.
    show_memories: bool,
    /// Form-field state for the "Add memory" widget.
    new_memory_name: String,
    new_memory_tag:  String,
    /// Last successful save. We debounce auto-saves to one every
    /// `SAVE_DEBOUNCE` so a lively UI session doesn't pound the
    /// SSD with TOML writes.
    last_save: Instant,
    /// Set whenever a user gesture changes a persisted field. The
    /// next post-debounce frame consumes the flag and writes.
    dirty:     bool,
}

/// Minimum interval between background TOML writes during a live
/// session. Quitting / disconnecting always saves immediately,
/// regardless of this debounce.
const SAVE_DEBOUNCE: Duration = Duration::from_secs(10);

impl ThetisApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_dark_theme(&cc.egui_ctx);

        // Load persisted settings. Failures here are non-fatal — fall
        // back to defaults so a corrupted thetis.toml never bricks
        // the app. The user can blow away ~/.config/thetis/ if they
        // really want a clean state.
        let settings = match Settings::load_default() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load settings, using defaults");
                let mut s = Settings::default();
                s.ensure_rx_slots(2);
                s
            }
        };

        // The HL2_IP env var takes precedence over the persisted IP
        // so a one-liner `HL2_IP=… cargo run` keeps working even
        // when the user has saved a different IP.
        let radio_ip = std::env::var("HL2_IP")
            .unwrap_or_else(|_| settings.general.last_radio_ip.clone());

        let mut rxs: Vec<RxView> = Vec::with_capacity(MAX_RX);
        for r in 0..MAX_RX {
            let serde_rx = settings.rxs.get(r).cloned().unwrap_or_default();
            rxs.push(RxView {
                enabled:      serde_rx.enabled || r == 0, // RX1 always defaults on
                frequency_hz: serde_rx.frequency_hz,
                mode:         mode_from_serde(serde_rx.mode),
                volume:       serde_rx.volume,
                nr3:          serde_rx.nr3,
                nr4:          serde_rx.nr4,
                waterfall:    Waterfall::new(),
            });
        }

        let band_stack = BandStack::from_settings(&settings.band_stacks);

        ThetisApp {
            radio:     None,
            telemetry: None,
            last_error: None,
            radio_ip,
            num_rx:    settings.general.num_rx.clamp(1, MAX_RX as u8),
            rxs,
            active_rx: settings.general.active_rx.clamp(0, MAX_RX as u8 - 1) as usize,
            band_stack,
            memories:        settings.memories,
            show_memories:   false,
            new_memory_name: String::new(),
            new_memory_tag:  String::new(),
            last_save:       Instant::now(),
            dirty:           false,
        }
    }

    /// Build a `Settings` snapshot from the current UI state. Used
    /// by both the debounced auto-save and the explicit save on
    /// connect / disconnect / quit.
    fn to_settings(&self) -> Settings {
        let mut s = Settings::default();
        s.ensure_rx_slots(MAX_RX);
        s.general = GeneralSettings {
            last_radio_ip: self.radio_ip.clone(),
            audio_device:  String::new(), // populated when B.5 audio picker lands
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
            };
        }
        s.band_stacks = self.band_stack.to_settings();
        s.memories    = self.memories.clone();
        s
    }

    /// Mark the in-memory settings dirty so the next debounce tick
    /// (or the next disconnect / quit) writes them to disk.
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Persist the current state to disk **right now**, regardless
    /// of the debounce. Used at connect / disconnect / app shutdown
    /// where we don't want to risk losing the most recent state.
    fn save_now(&mut self) {
        match self.to_settings().save_default() {
            Ok(()) => {
                self.dirty = false;
                self.last_save = Instant::now();
                tracing::debug!("settings saved");
            }
            Err(e) => tracing::warn!(error = %e, "settings save failed"),
        }
    }

    /// Auto-save path: only writes when something changed AND the
    /// debounce window has elapsed. Called once per UI frame from
    /// `App::ui`.
    fn maybe_autosave(&mut self) {
        if self.dirty && self.last_save.elapsed() >= SAVE_DEBOUNCE {
            self.save_now();
        }
    }

    /// Apply the stored entry for `band` to the active RX. Saves the
    /// active RX's current freq/mode back to its current band first
    /// so the user gets a true band-stack toggle (jump-away → jump-back
    /// preserves where you were).
    fn jump_to_band(&mut self, band: Band) {
        let rx = self.active_rx;
        if rx >= self.rxs.len() {
            return;
        }

        // 1. Snapshot current state into the matching band slot.
        let current_freq = self.rxs[rx].frequency_hz;
        let current_mode = self.rxs[rx].mode;
        if let Some(prev_band) = Band::for_freq(current_freq) {
            self.band_stack.set(prev_band, BandStackEntry {
                frequency_hz: current_freq,
                mode:         current_mode,
            });
        }

        // 2. Pull the destination band's last freq/mode (or its default).
        let entry = self.band_stack.get(band);
        self.rxs[rx].frequency_hz = entry.frequency_hz;
        self.rxs[rx].mode         = entry.mode;

        // 3. Push to the live radio if connected.
        if let Some(r) = &self.radio {
            let _ = r.set_rx_frequency(rx as u8, entry.frequency_hz);
            let _ = r.set_rx_mode(rx as u8, entry.mode);
        }
        self.mark_dirty();
    }

    fn connect(&mut self) {
        let addr_str = format!("{}:1024", self.radio_ip);
        let addr = match addr_str.parse() {
            Ok(a) => a,
            Err(e) => {
                self.last_error = Some(format!("invalid IP: {e}"));
                return;
            }
        };

        let mut config = RadioConfig {
            radio_addr:    addr,
            num_rx:        self.num_rx,
            audio_device:  None,
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
                self.telemetry = Some(r.telemetry());
                self.radio     = Some(r);
                self.last_error = None;
                // Connect snapshots the form fields the user just
                // committed (IP, num_rx, RX state) so a crash mid-
                // session keeps the most recent intent on disk.
                self.save_now();
            }
            Err(e) => {
                self.last_error = Some(format!("{e:#}"));
            }
        }
    }

    fn disconnect(&mut self) {
        if let Some(r) = self.radio.take() {
            let _ = r.stop();
        }
        self.telemetry = None;
        // Disconnect always saves so the next launch lands on the
        // same band / freq / mode the user left.
        self.save_now();
    }
}

impl eframe::App for ThetisApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep the UI animated even when the user isn't interacting —
        // the spectrum needs fresh draws at the DSP update rate (~23 Hz).
        ui.ctx().request_repaint_after(Duration::from_millis(40));

        // Debounced auto-save: only writes when something changed
        // and the SAVE_DEBOUNCE window has elapsed since the last
        // write. Cheap when there's nothing to do.
        self.maybe_autosave();

        // eframe 0.34 changed `App::update(&Context)` to
        // `App::ui(&mut Ui)`, marked `Panel::show` deprecated in
        // favour of `show_inside`, and replaced the old
        // `TopBottomPanel::top / ::bottom` constructors with
        // `Panel::top / ::bottom` on the unified `Panel` type.
        egui::Panel::top("top-bar").show_inside(ui, |ui| {
            self.draw_top_bar(ui);
        });

        egui::Panel::bottom("s-meter").show_inside(ui, |ui| {
            self.draw_s_meter(ui);
        });

        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.draw_main(ui, &ctx);
        });

        // Floating memories window. Drawn last so it overlays the
        // central panel; the user toggles it via the "Memories"
        // button in the top bar.
        if self.show_memories {
            self.draw_memories_window(&ctx);
        }
    }

    /// Final flush on window close. eframe calls this exactly once
    /// after the user closes the viewport, so it's the right place
    /// to disconnect the radio cleanly and persist the last state.
    fn on_exit(&mut self) {
        if self.radio.is_some() {
            self.disconnect();
        } else {
            self.save_now();
        }
    }
}

// --- UI sub-sections ----------------------------------------------------

impl ThetisApp {
    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        // Row 1: global session controls (Connect / IP / num_rx / status)
        ui.horizontal(|ui| {
            if self.radio.is_some() {
                if ui.button("Disconnect").clicked() {
                    self.disconnect();
                }
            } else if ui.button("Connect").clicked() {
                self.connect();
            }

            ui.separator();
            ui.label("IP:");
            let ip_resp = ui.add_enabled(
                self.radio.is_none(),
                egui::TextEdit::singleline(&mut self.radio_ip).desired_width(120.0),
            );
            if ip_resp.changed() {
                self.mark_dirty();
            }

            ui.separator();
            ui.label("RX:");
            // num_rx can only change while disconnected.
            let prev_num_rx = self.num_rx;
            ui.add_enabled_ui(self.radio.is_none(), |ui| {
                ui.radio_value(&mut self.num_rx, 1u8, "1");
                ui.radio_value(&mut self.num_rx, 2u8, "2");
            });
            if self.num_rx != prev_num_rx {
                self.mark_dirty();
            }

            ui.separator();
            ui.toggle_value(&mut self.show_memories, "Memories");

            ui.separator();
            self.draw_connection_status(ui);
        });

        // Row 2+: one "VFO bar" per configured RX.
        for rx in 0..self.num_rx as usize {
            ui.separator();
            self.draw_rx_row(ui, rx);
        }

        ui.separator();
        self.draw_band_buttons(ui);

        if let Some(e) = &self.last_error {
            ui.colored_label(Color32::LIGHT_RED, format!("error: {e}"));
        }
    }

    /// Floating "Memories" panel: scrollable list of named freq/mode
    /// bookmarks. Double-click a row to load it into the active RX,
    /// "Add" to capture the active RX's current state, "X" to delete.
    fn draw_memories_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_memories;
        let mut load_idx: Option<usize> = None;
        let mut delete_idx: Option<usize> = None;
        let mut add_clicked = false;

        egui::Window::new("Memories")
            .open(&mut open)
            .default_width(360.0)
            .default_height(380.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} memorie{}",
                    self.memories.len(),
                    if self.memories.len() == 1 { "" } else { "s" }
                ));
                ui.separator();

                // Capture form for new memory.
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_memory_name)
                            .desired_width(110.0),
                    );
                    ui.label("Tag:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_memory_tag)
                            .desired_width(110.0),
                    );
                    if ui.button("Add").clicked() {
                        add_clicked = true;
                    }
                });

                ui.separator();

                // Scrollable list of existing memories.
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, mem) in self.memories.iter().enumerate() {
                        ui.horizontal(|ui| {
                            // Double-click anywhere on the label loads
                            // the memory into the active RX.
                            let label = format!(
                                "{:<20} {:>10.3} MHz  {:?}",
                                mem.name,
                                mem.freq_hz as f64 / 1.0e6,
                                mem.mode,
                            );
                            let resp = ui.add(
                                egui::Label::new(egui::RichText::new(label).monospace())
                                    .sense(Sense::click()),
                            );
                            if resp.double_clicked() {
                                load_idx = Some(i);
                            }
                            if !mem.tag.is_empty() {
                                ui.weak(format!("({})", mem.tag));
                            }
                            // Push delete to the right edge.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("✕").on_hover_text("Delete").clicked() {
                                        delete_idx = Some(i);
                                    }
                                },
                            );
                        });
                    }
                });
            });

        self.show_memories = open;

        if add_clicked {
            self.add_current_as_memory();
        }
        if let Some(i) = load_idx {
            self.load_memory(i);
        }
        if let Some(i) = delete_idx {
            self.memories.remove(i);
            self.mark_dirty();
        }
    }

    /// Capture the active RX's current frequency + mode as a new memory.
    /// Uses the form-field name/tag, falling back to a "{freq:.3} MHz"
    /// auto-name when the user left the name blank.
    fn add_current_as_memory(&mut self) {
        let rx = self.active_rx;
        if rx >= self.rxs.len() {
            return;
        }
        let view = &self.rxs[rx];
        let name = if self.new_memory_name.trim().is_empty() {
            format!("{:.3} MHz", view.frequency_hz as f64 / 1.0e6)
        } else {
            self.new_memory_name.trim().to_string()
        };
        self.memories.push(Memory {
            name,
            freq_hz: view.frequency_hz,
            mode:    mode_to_serde(view.mode),
            tag:     self.new_memory_tag.trim().to_string(),
        });
        self.new_memory_name.clear();
        self.new_memory_tag.clear();
        self.mark_dirty();
    }

    /// Apply memory `i` to the active RX (frequency + mode), pushing
    /// to the live radio if connected.
    fn load_memory(&mut self, i: usize) {
        let Some(mem) = self.memories.get(i).cloned() else { return };
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

    /// Horizontal row of amateur-band buttons. Pressing one routes
    /// through `jump_to_band(band)` which snapshots the active RX's
    /// current freq/mode into the matching band slot, then loads
    /// the new band's entry. The currently-tuned band is highlighted.
    fn draw_band_buttons(&mut self, ui: &mut egui::Ui) {
        let active_freq = self
            .rxs
            .get(self.active_rx)
            .map(|v| v.frequency_hz)
            .unwrap_or(0);
        let current_band = Band::for_freq(active_freq);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Band:").strong());
            ui.label(format!("(RX{})", self.active_rx + 1));
            for band in Band::ALL {
                let is_current = current_band == Some(band);
                let mut text = egui::RichText::new(band.label()).monospace();
                if is_current {
                    text = text.color(Color32::BLACK).background_color(Color32::LIGHT_GREEN);
                }
                if ui.button(text).clicked() {
                    self.jump_to_band(band);
                }
            }
        });
    }

    fn draw_rx_row(&mut self, ui: &mut egui::Ui, rx: usize) {
        let rx_u8 = rx as u8;
        ui.horizontal(|ui| {
            let label = format!("RX{}:", rx + 1);
            ui.label(egui::RichText::new(label).strong());

            // Enable toggle
            let mut enabled = self.rxs[rx].enabled;
            let enabled_changed = ui.checkbox(&mut enabled, "on").changed();
            if enabled_changed {
                self.rxs[rx].enabled = enabled;
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_enabled(rx_u8, enabled);
                }
                self.mark_dirty();
            }

            ui.separator();
            ui.label("VFO:");
            let mut freq = self.rxs[rx].frequency_hz as f64;
            let changed = ui
                .add(
                    egui::DragValue::new(&mut freq)
                        .range(0.0..=60_000_000.0)
                        .speed(10.0)
                        .suffix(" Hz"),
                )
                .changed();
            if changed {
                self.rxs[rx].frequency_hz = freq.max(0.0) as u32;
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_frequency(rx_u8, self.rxs[rx].frequency_hz);
                }
                self.mark_dirty();
            }
            ui.label(format!("({:.3} MHz)", self.rxs[rx].frequency_hz as f64 / 1.0e6));

            ui.separator();
            ui.label("Mode:");
            let prev_mode = self.rxs[rx].mode;
            egui::ComboBox::from_id_salt(("mode", rx))
                .selected_text(format!("{:?}", self.rxs[rx].mode))
                .show_ui(ui, |ui| {
                    for m in [
                        WdspMode::Lsb, WdspMode::Usb, WdspMode::Am, WdspMode::Sam,
                        WdspMode::Fm, WdspMode::CwL, WdspMode::CwU,
                        WdspMode::DigL, WdspMode::DigU,
                    ] {
                        ui.selectable_value(&mut self.rxs[rx].mode, m, format!("{m:?}"));
                    }
                });
            if self.rxs[rx].mode != prev_mode {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_mode(rx_u8, self.rxs[rx].mode);
                }
                self.mark_dirty();
            }

            ui.separator();
            ui.label("Vol:");
            let prev_vol = self.rxs[rx].volume;
            ui.add(egui::Slider::new(&mut self.rxs[rx].volume, 0.0..=2.0).show_value(true));
            if (self.rxs[rx].volume - prev_vol).abs() > f32::EPSILON {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_volume(rx_u8, self.rxs[rx].volume);
                }
                self.mark_dirty();
            }

            ui.separator();
            let prev_nr3 = self.rxs[rx].nr3;
            ui.checkbox(&mut self.rxs[rx].nr3, "NR3");
            if self.rxs[rx].nr3 != prev_nr3 {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_nr3(rx_u8, self.rxs[rx].nr3);
                }
                self.mark_dirty();
            }
            let prev_nr4 = self.rxs[rx].nr4;
            ui.checkbox(&mut self.rxs[rx].nr4, "NR4");
            if self.rxs[rx].nr4 != prev_nr4 {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_nr4(rx_u8, self.rxs[rx].nr4);
                }
                self.mark_dirty();
            }
        });
    }

    fn draw_connection_status(&self, ui: &mut egui::Ui) {
        match (&self.radio, &self.telemetry) {
            (Some(r), Some(_)) => {
                let s = r.status();
                let connected = s.session.is_connected(Instant::now());
                let dot = if connected { "●" } else { "○" };
                let colour = if connected { Color32::GREEN } else { Color32::GRAY };
                ui.colored_label(colour, dot);
                ui.label(format!(
                    "pkts {}  dsp {}k  audio {}k  underruns {}",
                    s.session.packets_received,
                    s.samples_dsp / 1000,
                    s.samples_audio / 1000,
                    s.audio_underruns,
                ));
            }
            _ => {
                ui.colored_label(Color32::GRAY, "○ disconnected");
            }
        }
    }

    fn draw_main(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(telem) = &self.telemetry else {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("Not connected");
                ui.label(
                    "Set the radio IP + RX count in the top bar and click Connect.",
                );
            });
            return;
        };

        let snapshot = telem.load_full();
        let num_rx = snapshot.num_rx.min(MAX_RX as u8) as usize;
        if num_rx == 0 {
            return;
        }

        // Push fresh rows into each RX's waterfall *before* drawing so
        // the visual and the data stay in sync.
        for r in 0..num_rx {
            self.rxs[r].waterfall.push_row(&snapshot.rx[r].spectrum_bins_db);
        }

        // Divide the central area into `num_rx` horizontal bands.
        // Each band gets its own spectrum on top and waterfall below.
        let avail = ui.available_size();
        let band_h = (avail.y / num_rx as f32).max(240.0);

        // Pending tune commands collected during the draw pass; applied
        // after the immutable telemetry borrow is released so we can
        // mutably touch self.radio / self.rxs.
        let mut pending_tunes: Vec<(usize, u32)> = Vec::new();
        // RX whose spectrum/waterfall received the most recent click;
        // promoted to `active_rx` after the draw closure.
        let mut newly_active: Option<usize> = None;

        ui.vertical(|ui| {
            for r in 0..num_rx {
                if r > 0 {
                    ui.separator();
                }
                let (band_rect, _) = ui.allocate_exact_size(
                    Vec2::new(avail.x, band_h - 8.0),
                    Sense::hover(),
                );
                // Split vertically: top 35% spectrum, bottom 65% waterfall.
                let spec_h  = (band_rect.height() * 0.35).max(80.0);
                let water_h = (band_rect.height() - spec_h - 4.0).max(60.0);
                let spec_rect = Rect::from_min_size(
                    band_rect.min,
                    Vec2::new(band_rect.width(), spec_h),
                );
                let water_rect = Rect::from_min_size(
                    Pos2::new(band_rect.min.x, band_rect.min.y + spec_h + 4.0),
                    Vec2::new(band_rect.width(), water_h),
                );

                draw_spectrum(ui, spec_rect, &snapshot.rx[r].spectrum_bins_db);
                // Passband overlay — translucent rectangle showing
                // where the demod filter passes audio. Drawn on top
                // of the spectrum line so the bins under the filter
                // remain visible.
                let (lo, hi) = snapshot.rx[r].mode.default_passband_hz();
                draw_passband_overlay(
                    ui, spec_rect,
                    snapshot.rx[r].span_hz,
                    lo as f32, hi as f32,
                );
                // RX label in the corner of the spectrum rect
                ui.painter_at(spec_rect).text(
                    spec_rect.min + Vec2::new(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    format!("RX{}  {:.3} MHz  {:?}",
                        r + 1,
                        snapshot.rx[r].center_freq_hz as f64 / 1.0e6,
                        snapshot.rx[r].mode),
                    egui::FontId::monospace(12.0),
                    if snapshot.rx[r].enabled {
                        Color32::LIGHT_GREEN
                    } else {
                        Color32::GRAY
                    },
                );

                self.rxs[r].waterfall.draw(ui, ctx, water_rect);

                // VFO marker — vertical line at rect center on both
                // panels. The DDC is always centered on the VFO so the
                // marker is anchored to the geometric middle and the
                // spectrum scrolls under it when the user tunes.
                draw_vfo_marker(ui, spec_rect);
                draw_vfo_marker(ui, water_rect);

                // Click-to-tune + wheel-tune. Catch the same input on
                // both rects so the user can interact with whichever
                // one is closer to the cursor.
                let center_hz = snapshot.rx[r].center_freq_hz;
                let span_hz   = snapshot.rx[r].span_hz;
                let (new_freq, clicked) = handle_tune_input(
                    ui, spec_rect,
                    egui::Id::new(("spec-tune", r)),
                    center_hz, span_hz,
                );
                if let Some(f) = new_freq { pending_tunes.push((r, f)); }
                if clicked { newly_active = Some(r); }

                let (new_freq, clicked) = handle_tune_input(
                    ui, water_rect,
                    egui::Id::new(("water-tune", r)),
                    center_hz, span_hz,
                );
                if let Some(f) = new_freq { pending_tunes.push((r, f)); }
                if clicked { newly_active = Some(r); }
            }
        });

        // Apply tune commands after the draw closure to satisfy the
        // borrow checker (snapshot above is still alive inside ui.vertical).
        let any_change = !pending_tunes.is_empty();
        for (rx, new_freq) in pending_tunes {
            self.rxs[rx].frequency_hz = new_freq;
            if let Some(r) = &self.radio {
                let _ = r.set_rx_frequency(rx as u8, new_freq);
            }
        }
        if let Some(rx) = newly_active {
            if self.active_rx != rx {
                self.active_rx = rx;
                self.mark_dirty();
            }
        }
        if any_change {
            self.mark_dirty();
        }
    }

    fn draw_s_meter(&self, ui: &mut egui::Ui) {
        let Some(telem) = &self.telemetry else {
            ui.horizontal(|ui| {
                ui.monospace("S-meter: --");
            });
            return;
        };
        let snapshot = telem.load_full();
        let num_rx = snapshot.num_rx.min(MAX_RX as u8) as usize;

        ui.vertical(|ui| {
            for r in 0..num_rx {
                draw_s_meter_scaled(ui, r, snapshot.rx[r].s_meter_db);
            }
        });
    }
}

/// Convert a dBFS reading from the DSP to dBm at the antenna,
/// assuming a 50 Ω termination and the standard SDR calibration of
/// `S9 = -73 dBm`. The actual hardware offset is band- and gain-
/// dependent; this constant is the phase B placeholder until B.5
/// adds a per-band calibration table.
const SMETER_DBFS_TO_DBM_OFFSET: f32 = 73.0;

/// Map a dBm reading to its IARU S-unit number on HF.
/// 6 dB per S-unit between S1 and S9, then over-S9 in 10 dB steps
/// (S9+10, +20, +40, +60). Returns a fractional value so the needle
/// moves smoothly between integer units.
fn dbm_to_s_units(dbm: f32) -> f32 {
    // S9 = -73 dBm; S1 = -127 dBm.
    if dbm <= -73.0 {
        ((dbm + 127.0) / 6.0).clamp(0.0, 9.0)
    } else {
        9.0 + (dbm + 73.0) / 10.0
    }
}

fn draw_s_meter_scaled(ui: &mut egui::Ui, rx: usize, dbfs: f32) {
    let dbm = dbfs - SMETER_DBFS_TO_DBM_OFFSET;
    let s = dbm_to_s_units(dbm);

    ui.horizontal(|ui| {
        ui.monospace(format!("RX{}", rx + 1));

        let bar_width = (ui.available_width() - 200.0).max(180.0);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(bar_width, 18.0),
            Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, Color32::from_gray(28));

        // S1..S9 occupies the left 60% of the bar; +10..+60 the
        // remaining 40%. Same proportions as a typical analog
        // S-meter scale, so the needle position matches a user's
        // muscle memory from a hardware rig.
        let s9_split = 0.6_f32;
        let s_norm = if s <= 9.0 {
            (s / 9.0) * s9_split
        } else {
            s9_split + ((s - 9.0) / 6.0).clamp(0.0, 1.0) * (1.0 - s9_split)
        };

        // Filled portion.
        let filled = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width() * s_norm, rect.height()),
        );
        painter.rect_filled(filled, 2.0, level_color(dbfs));

        // Tick marks: S1..S9 (every unit) + +10/+20/+40/+60.
        let tick_color = Color32::from_gray(120);
        for i in 1..=9 {
            let t = (i as f32 / 9.0) * s9_split;
            let x = rect.min.x + t * rect.width();
            painter.line_segment(
                [Pos2::new(x, rect.max.y - 5.0), Pos2::new(x, rect.max.y)],
                Stroke::new(1.0, tick_color),
            );
        }
        for (i, _label) in [10, 20, 40, 60].iter().enumerate() {
            let frac = (i + 1) as f32 / 4.0;
            let t = s9_split + frac * (1.0 - s9_split);
            let x = rect.min.x + t * rect.width();
            painter.line_segment(
                [Pos2::new(x, rect.max.y - 5.0), Pos2::new(x, rect.max.y)],
                Stroke::new(1.0, Color32::from_rgb(180, 100, 80)),
            );
        }

        // Numeric readout: integer S-unit if ≤ S9, otherwise S9+xx dB.
        let readout = if s <= 9.0 {
            format!("S{:.0}", s.round())
        } else {
            format!("S9+{:.0}", (dbm + 73.0).max(0.0))
        };
        ui.monospace(format!("{:<7}{:>6.1} dBm", readout, dbm));
    });
}

// --- Settings <-> UI conversion -----------------------------------------

/// Round-trip helper: turn `wdsp::Mode` into the serde-friendly mirror
/// in `thetis-settings`. Variants line up 1:1 so the match is total
/// without a fallback.
fn mode_to_serde(m: WdspMode) -> SerdeMode {
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

fn mode_from_serde(m: SerdeMode) -> WdspMode {
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

// --- Theme ---------------------------------------------------------------

/// Tweak egui's stock dark visuals to give the radio more contrast
/// without shipping a custom font (which would mean adding ~2 MB of
/// .ttf to the repo). The defaults are fine for general apps but a
/// little washed-out next to a colourful spectrum/waterfall.
fn apply_dark_theme(ctx: &egui::Context) {
    // egui 0.34 split per-context and global styles. We want a
    // process-wide tweak applied at startup, so use the global API
    // (`global_style` + `set_global_style`) rather than the
    // per-context `style()` which is now reserved for transient
    // overrides inside a `ui.with_style(...)` scope.
    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::dark();

    // Bigger monospace so the VFO bar is readable from across the
    // room — same idea as the giant 7-segment digits on a hardware
    // rig. Heading also pumped up for the "Not connected" splash.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(TextStyle::Monospace, FontId::new(15.0, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Button,    FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body,      FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Heading,   FontId::new(20.0, FontFamily::Proportional));

    // Darker overall background, brighter accent for selected /
    // active widgets so the connect/disconnect state is obvious at
    // a glance.
    style.visuals.window_fill        = Color32::from_rgb(18, 20, 24);
    style.visuals.panel_fill         = Color32::from_rgb(22, 24, 28);
    style.visuals.extreme_bg_color   = Color32::from_rgb(10, 12, 14);
    style.visuals.widgets.noninteractive.bg_stroke =
        Stroke::new(1.0, Color32::from_gray(60));
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(40, 44, 50);
    style.visuals.widgets.hovered.bg_fill  = Color32::from_rgb(60, 80, 100);
    style.visuals.widgets.active.bg_fill   = Color32::from_rgb(80, 140, 180);
    style.visuals.selection.bg_fill        = Color32::from_rgb(40, 100, 160);
    style.visuals.hyperlink_color          = Color32::from_rgb(120, 200, 255);

    // Tighter spacing — radio control panels are dense, the egui
    // defaults waste vertical space.
    style.spacing.item_spacing       = Vec2::new(6.0, 4.0);
    style.spacing.button_padding     = Vec2::new(8.0, 3.0);
    style.spacing.interact_size      = Vec2::new(20.0, 22.0);

    ctx.set_global_style(style);
}

// --- Amateur bands -------------------------------------------------------

/// HF + 6 m amateur bands recognised by the band-button row.
///
/// Order matches the conventional band-stack layout in commercial
/// SDR control software (low → high frequency, left → right).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            Band::M60  => ( 5_330_000,  5_410_000), // US 60m channels
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
    fn default_entry(self) -> BandStackEntry {
        let (freq, mode) = match self {
            Band::M160 => ( 1_840_000, WdspMode::Lsb),
            Band::M80  => ( 3_573_000, WdspMode::Usb), // FT8
            Band::M60  => ( 5_357_000, WdspMode::Usb),
            Band::M40  => ( 7_074_000, WdspMode::Usb), // FT8
            Band::M30  => (10_136_000, WdspMode::Usb), // FT8
            Band::M20  => (14_074_000, WdspMode::Usb), // FT8
            Band::M17  => (18_100_000, WdspMode::Usb),
            Band::M15  => (21_074_000, WdspMode::Usb), // FT8
            Band::M12  => (24_915_000, WdspMode::Usb), // FT8
            Band::M10  => (28_074_000, WdspMode::Usb), // FT8
            Band::M6   => (50_313_000, WdspMode::Usb), // FT8
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
/// type stays serialisable for B.5.
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
    /// from `thetis.toml`. Missing entries fall back to the band's
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

// --- Tuning interaction --------------------------------------------------

/// Draw a thin vertical line at the horizontal center of `rect` to
/// mark the current VFO position. The DDC is always tuned to the
/// spectrum's center, so the marker is purely geometric.
fn draw_vfo_marker(ui: &egui::Ui, rect: Rect) {
    let x = rect.center().x;
    ui.painter_at(rect).line_segment(
        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
        Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 160)),
    );
}

/// Handle click-to-tune + wheel-tune over a spectrum or waterfall
/// rect. Returns `(new_freq, clicked)`:
/// - `new_freq`: `Some(hz)` when the user issues a tuning gesture,
/// - `clicked`:  `true` when the rect was clicked at all (used by the
///   caller to promote this RX to the "active" one for band buttons).
///
/// Gestures:
/// - Left click / drag → jump VFO so the clicked frequency becomes
///   the new center.
/// - Mouse wheel       → ±10 Hz per notch (fine tune)
/// - Shift + wheel     → ±100 Hz per notch
/// - Ctrl + wheel      → ±1 kHz per notch (fast scan)
fn handle_tune_input(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    center_hz: u32,
    span_hz: u32,
) -> (Option<u32>, bool) {
    let response = ui.interact(rect, id, Sense::click_and_drag());

    let mut new_freq: Option<u32> = None;
    let clicked = response.clicked() || response.dragged();

    // Click / drag → frequency under cursor becomes the new center.
    if clicked {
        if let Some(pos) = response.interact_pointer_pos() {
            let dx_norm = (pos.x - rect.center().x) / rect.width(); // -0.5..+0.5
            let delta_hz = (dx_norm * span_hz as f32).round() as i64;
            let next = (center_hz as i64 + delta_hz).max(0) as u32;
            if next != center_hz {
                new_freq = Some(next);
            }
        }
    }

    // Wheel tuning when hovering. Each tick of the wheel maps to a
    // fixed frequency step (10 / 100 / 1000 Hz depending on modifiers)
    // rather than scaling with the panel size, so it's predictable.
    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5 {
            let modifiers = ui.input(|i| i.modifiers);
            let step_hz: i64 = if modifiers.ctrl {
                1000
            } else if modifiers.shift {
                100
            } else {
                10
            };
            // egui reports +Y for "scrolled up" → tune up.
            let ticks = (scroll_y / 50.0).round() as i64;
            let ticks = if ticks == 0 {
                if scroll_y > 0.0 { 1 } else { -1 }
            } else {
                ticks
            };
            let base = new_freq.unwrap_or(center_hz) as i64;
            let next = (base + ticks * step_hz).max(0) as u32;
            if next as i64 != base {
                new_freq = Some(next);
            }
        }
    }

    (new_freq, clicked)
}

// --- Passband overlay ----------------------------------------------------

/// Tint the portion of the spectrum that the demod filter passes
/// audio through. Coordinates `f_lo` / `f_hi` are baseband Hz
/// (i.e. relative to the VFO/center, with USB positive and LSB
/// negative); we map them onto the rect's x-axis using the same
/// `[center - span/2 .. center + span/2]` mapping the click-tune
/// helper uses.
fn draw_passband_overlay(ui: &egui::Ui, rect: Rect, span_hz: u32, f_lo: f32, f_hi: f32) {
    if span_hz == 0 || f_hi <= f_lo {
        return;
    }
    let span = span_hz as f32;
    let to_x = |hz: f32| -> f32 {
        let t = (hz / span) + 0.5; // 0..1
        rect.min.x + t.clamp(0.0, 1.0) * rect.width()
    };
    let x0 = to_x(f_lo);
    let x1 = to_x(f_hi);
    if (x1 - x0).abs() < 1.0 {
        return;
    }
    let band = Rect::from_min_max(
        Pos2::new(x0, rect.min.y),
        Pos2::new(x1, rect.max.y),
    );
    ui.painter_at(rect).rect_filled(
        band,
        0.0,
        Color32::from_rgba_premultiplied(80, 200, 255, 30),
    );
}

// --- Spectrum (immediate-mode line draw) --------------------------------

fn draw_spectrum(ui: &mut egui::Ui, rect: Rect, bins_db: &[f32]) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(8, 10, 14));

    if bins_db.is_empty() {
        return;
    }

    // Draw horizontal grid every 20 dB between -120 and 0, with a
    // brighter band at -60 dB to help eyeball "loud signal" / "S9".
    for db in (-120..=0).step_by(20) {
        let y = db_to_y(db as f32, rect);
        let color = if db == -60 {
            Color32::from_rgb(60, 80, 60)
        } else {
            Color32::from_gray(48)
        };
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, color),
        );
    }

    // Map FFT bins horizontally across the rect and plot as a polyline.
    // Brighter green than `LIGHT_GREEN` so the trace pops on the dark
    // background even with the passband overlay drawn over it.
    let n = bins_db.len();
    let mut points = Vec::with_capacity(n);
    for (i, &db) in bins_db.iter().enumerate() {
        let x = rect.min.x + (i as f32 / (n - 1) as f32) * rect.width();
        let y = db_to_y(db, rect);
        points.push(Pos2::new(x, y));
    }
    painter.add(egui::Shape::line(
        points,
        Stroke::new(1.6, Color32::from_rgb(140, 255, 160)),
    ));
}

/// Map a dBFS value to a y-coordinate inside `rect`. -120 dB is at the
/// bottom, 0 dB at the top.
fn db_to_y(db: f32, rect: Rect) -> f32 {
    let clamped = db.clamp(-120.0, 0.0);
    let t       = (clamped + 120.0) / 120.0; // 0..1 (0 = bottom)
    rect.max.y - t * rect.height()
}

/// Colour for the S-meter fill, easing from cool green through yellow
/// to red as the signal gets stronger.
fn level_color(db: f32) -> Color32 {
    if db < -80.0       { Color32::from_rgb(40, 120, 40) }
    else if db < -50.0  { Color32::from_rgb(80, 180, 80) }
    else if db < -20.0  { Color32::from_rgb(200, 200, 80) }
    else                { Color32::from_rgb(220, 80, 60) }
}

// --- Waterfall ----------------------------------------------------------

/// Scrolling waterfall display, rendered as a CPU-side `ColorImage` that
/// gets uploaded to a persistent `TextureHandle` every frame.
///
/// Layout: rows are the time axis (row 0 = oldest, row `HEIGHT-1` = newest).
/// Each row holds `SPECTRUM_BINS` RGBA pixels. Pushing a row shifts all
/// previous rows up by one (memmove) and writes the new one at the bottom.
///
/// A 1024 × 256 waterfall is ~1 MB. At ~25 frames per second the
/// per-frame memmove overhead is well under 1 ms on any modern CPU.
struct Waterfall {
    /// Flat RGBA buffer; `pixels[y * WIDTH + x]` addresses row y, col x.
    pixels:  Vec<Color32>,
    texture: Option<TextureHandle>,
}

impl Waterfall {
    const WIDTH:  usize = SPECTRUM_BINS;
    const HEIGHT: usize = 256;

    fn new() -> Self {
        Waterfall {
            pixels:  vec![Color32::BLACK; Self::WIDTH * Self::HEIGHT],
            texture: None,
        }
    }

    fn push_row(&mut self, bins_db: &[f32]) {
        // Shift the whole buffer up by one row of pixels; the bottom
        // row is then overwritten with the new data. This is the
        // conventional "SDR waterfall scrolls downward" layout — in
        // viewing coords the newest data appears at the top, older
        // data drifts down.
        let row_len = Self::WIDTH;
        self.pixels.copy_within(row_len.., 0);

        let new_row_start = (Self::HEIGHT - 1) * row_len;
        let new_row = &mut self.pixels[new_row_start..new_row_start + row_len];
        let n = bins_db.len().min(row_len);
        for (i, px) in new_row.iter_mut().enumerate().take(n) {
            let src_idx = (i * bins_db.len()) / row_len;
            *px = db_to_waterfall_color(bins_db[src_idx]);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, rect: Rect) {
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);

        let image = ColorImage {
            size:   [Self::WIDTH, Self::HEIGHT],
            pixels: self.pixels.clone(),
            source_size: Vec2::new(Self::WIDTH as f32, Self::HEIGHT as f32),
        };
        let tex = self.texture.get_or_insert_with(|| {
            ctx.load_texture("waterfall", image.clone(), TextureOptions::LINEAR)
        });
        tex.set(image, TextureOptions::LINEAR);

        ui.painter_at(rect).image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Simple two-stop colour ramp from black → blue → cyan → yellow → red
/// as signal strength climbs. -120 dBFS = black, 0 dBFS = bright red.
fn db_to_waterfall_color(db: f32) -> Color32 {
    let t = ((db + 120.0) / 120.0).clamp(0.0, 1.0);
    if t < 0.25 {
        // black → blue
        let b = (t / 0.25 * 255.0) as u8;
        Color32::from_rgb(0, 0, b)
    } else if t < 0.5 {
        // blue → cyan
        let g = ((t - 0.25) / 0.25 * 255.0) as u8;
        Color32::from_rgb(0, g, 255)
    } else if t < 0.75 {
        // cyan → yellow
        let r = ((t - 0.5) / 0.25 * 255.0) as u8;
        let b = 255 - r;
        Color32::from_rgb(r, 255, b)
    } else {
        // yellow → red
        let g = 255 - ((t - 0.75) / 0.25 * 255.0) as u8;
        Color32::from_rgb(255, g, 0)
    }
}
