//! egui + wgpu desktop frontend for Thetis.
//!
//! This crate is a **humble view** in the MVVM split. The application's
//! state, event handling, persistence, and radio interactions all live
//! in [`thetis_app::App`]. This crate's job is exclusively:
//!
//! 1. Read from `&App` to render egui widgets
//! 2. Translate user gestures (clicks, drags, scrolls) into
//!    `App::set_*` / `App::toggle_*` calls
//! 3. Cache the per-RX waterfall textures (egui-specific resource)
//! 4. Own the eframe entry point + apply our dark theme
//!
//! No application logic should live here. If you find yourself wanting
//! to add a `mark_dirty` or push a `DspCommand` from this crate, the
//! method belongs in `thetis-app` instead.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};

use thetis_app::{
    dbm_to_s_units, mode_to_serde, App, AppOptions, Band, FilterPreset, WindowKind,
    SMETER_DBFS_TO_DBM_OFFSET,
};
use thetis_core::{WdspMode, MAX_RX, SPECTRUM_BINS};
use thetis_script::{ReplLineKind, ScriptEngine};
use thetis_settings::Memory;

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
        Box::new(|cc| Ok(Box::new(EguiView::new(cc)))),
    )
}

// --------------------------------------------------------------------
// EguiView: the humble view
// --------------------------------------------------------------------

/// Frontend wrapper. Owns the [`App`] view-model plus per-RX waterfall
/// texture caches (egui-specific resources that can't live in `App`)
/// and a couple of transient form-field strings for the Memories
/// window's "Add" form.
/// Per-RX spectrum overlay state (peak hold + averaging).
/// Lives in EguiView because it's frontend-specific rendering state.
struct SpectrumOverlay {
    peak_bins:    Vec<f32>,
    avg_bins:     Vec<f32>,
    show_peak:    bool,
    show_avg:     bool,
}

impl SpectrumOverlay {
    fn new() -> Self {
        SpectrumOverlay {
            peak_bins: vec![-140.0; SPECTRUM_BINS],
            avg_bins:  vec![-140.0; SPECTRUM_BINS],
            show_peak: false,
            show_avg:  false,
        }
    }

    fn update(&mut self, bins_db: &[f32]) {
        let n = bins_db.len().min(self.peak_bins.len());
        for (i, &db) in bins_db.iter().enumerate().take(n) {
            if db > self.peak_bins[i] {
                self.peak_bins[i] = db;
            } else {
                self.peak_bins[i] -= 0.3;
            }
            self.avg_bins[i] = self.avg_bins[i] * 0.85 + db * 0.15;
        }
    }
}

pub struct EguiView {
    app: App,
    /// Per-RX waterfall texture cache. Indexed 0..MAX_RX.
    waterfalls: Vec<Waterfall>,
    /// Per-RX spectrum overlay (peak hold + averaging). Indexed 0..MAX_RX.
    overlays: Vec<SpectrumOverlay>,
    /// Transient form-field state for the "Add memory" widget. Lives
    /// here (not in `App`) because it's tied to the egui form
    /// lifecycle and would be re-created from scratch by another
    /// frontend.
    new_memory_name: String,
    new_memory_tag:  String,
    /// Active tab index in the Setup window.
    setup_tab: usize,
    /// Rhai scripting engine + REPL state.
    script_engine: ScriptEngine,
    /// REPL input field.
    repl_input: String,
}

impl EguiView {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_dark_theme(&cc.egui_ctx);

        // The HL2_IP env var takes precedence over the persisted IP
        // so a one-liner `HL2_IP=… cargo run` keeps working even
        // when the user has saved a different IP.
        let opts = AppOptions {
            radio_ip_override: std::env::var("HL2_IP").ok(),
        };
        let app = App::new(opts);

        let waterfalls = (0..MAX_RX).map(|_| Waterfall::new()).collect();
        let overlays   = (0..MAX_RX).map(|_| SpectrumOverlay::new()).collect();

        EguiView {
            app,
            waterfalls,
            overlays,
            new_memory_name: String::new(),
            new_memory_tag:  String::new(),
            setup_tab:       0,
            script_engine:   ScriptEngine::default(),
            repl_input:      String::new(),
        }
    }
}

impl eframe::App for EguiView {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep the UI animated even when the user isn't interacting —
        // the spectrum needs fresh draws at the DSP update rate (~23 Hz).
        ui.ctx().request_repaint_after(Duration::from_millis(40));

        // Per-frame app tick: drives debounced auto-save (and, in
        // phase D.12, the script scheduler + event bus).
        self.app.tick(Instant::now());

        // --- Thetis-style panel layout (D.1) ---
        //
        // Panel ordering matters in egui: panels are laid out in
        // declaration order, and each one claims space from the
        // remaining available rect. CentralPanel must be last.
        //
        // ┌─────────────────────────────────────────────────┐
        // │  TopPanel: VFO bars + connect + band buttons     │
        // ├──────────────────────────────────┬──────────────┤
        // │                                  │  SidePanel R │
        // │   CentralPanel: spectrum +       │  Mode        │
        // │   waterfall (resizable split)    │  Band        │
        // │                                  │  Filter      │
        // ├──────────────────────────────────┴──────────────┤
        // │  BottomPanel: S-meter + DSP controls             │
        // ├─────────────────────────────────────────────────┤
        // │  StatusBar: pkts/dsp/audio/underruns              │
        // └─────────────────────────────────────────────────┘

        // 1a. Menu bar (File / View / Help)
        egui::Panel::top("menu-bar").show_inside(ui, |ui| {
            self.draw_menu_bar(ui);
        });

        // 1b. Toolbar + VFO rows
        egui::Panel::top("top-bar").show_inside(ui, |ui| {
            self.draw_top_bar(ui);
        });

        // 2. Status bar (bottom-most, thin, not resizable)
        egui::Panel::bottom("status-bar")
            .show_inside(ui, |ui| {
                self.draw_status_bar(ui);
            });

        // 3. Bottom controls panel: VFO ctrls | DSP | Display | Mode-specific
        egui::Panel::bottom("controls")
            .resizable(true)
            .min_size(50.0)
            .max_size(300.0)
            .default_size(80.0)
            .show_inside(ui, |ui| {
                self.draw_bottom_panel(ui);
            });

        // 4. Right side panel: Mode + Band + Filter
        egui::Panel::right("side-panel")
            .resizable(true)
            .min_size(140.0)
            .max_size(300.0)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                self.draw_side_panel(ui);
            });

        // 5. Central panel: spectrum + waterfall (takes remaining space)
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.draw_main(ui, &ctx);
        });

        // Floating windows go last so they overlay the central panel.
        if self.app.window_open(WindowKind::Memories) {
            self.draw_memories_window(&ctx);
        }
        if self.app.window_open(WindowKind::BandStack) {
            self.draw_band_stack_window(&ctx);
        }
        if self.app.window_open(WindowKind::Multimeter) {
            self.draw_multimeter_window(&ctx);
        }
        if self.app.window_open(WindowKind::Eq) {
            self.draw_eq_window(&ctx);
        }
        if self.app.window_open(WindowKind::Repl) {
            self.draw_repl_window(&ctx);
        }
        if self.app.window_open(WindowKind::Setup) {
            self.draw_setup_window(&ctx);
        }
    }

    /// Final flush on window close. eframe calls this exactly once
    /// after the user closes the viewport, so it's the right place
    /// to disconnect the radio cleanly and persist the last state.
    fn on_exit(&mut self) {
        self.app.shutdown();
    }
}

// --- UI sub-sections ----------------------------------------------------

impl EguiView {
    /// Right side panel: Mode selector + Band selector + Filter presets.
    /// Replaces the old in-row ComboBox mode picker and the inline band
    /// buttons — these now live in their own dedicated right-hand column
    /// matching the Thetis upstream layout.
    fn draw_side_panel(&mut self, ui: &mut egui::Ui) {
        let active_rx = self.app.active_rx();
        let rx_u8 = active_rx as u8;

        // --- Mode selector ---
        egui::CollapsingHeader::new(egui::RichText::new("Mode").strong())
            .default_open(true)
            .show(ui, |ui| {
                let current_mode = self.app.rx(active_rx).map(|r| r.mode).unwrap_or(WdspMode::Usb);
                let modes = [
                    (WdspMode::Lsb,  "LSB"),  (WdspMode::Usb,  "USB"),
                    (WdspMode::CwL,  "CWL"),  (WdspMode::CwU,  "CWU"),
                    (WdspMode::Am,   "AM"),   (WdspMode::Sam,  "SAM"),
                    (WdspMode::Dsb,  "DSB"),  (WdspMode::Fm,   "FM"),
                    (WdspMode::DigL, "DIGL"), (WdspMode::DigU, "DIGU"),
                    (WdspMode::Drm,  "DRM"),  (WdspMode::Spec, "SPEC"),
                ];
                ui.columns(2, |cols| {
                    for (i, &(mode, label)) in modes.iter().enumerate() {
                        let col = &mut cols[i % 2];
                        let is_selected = mode == current_mode;
                        let text = if is_selected {
                            egui::RichText::new(label).strong().color(Color32::BLACK)
                                .background_color(Color32::LIGHT_GREEN)
                        } else {
                            egui::RichText::new(label).monospace()
                        };
                        if col.selectable_label(is_selected, text).clicked() && mode != current_mode {
                            self.app.set_rx_mode(rx_u8, mode);
                        }
                    }
                });
            });

        ui.separator();

        // --- Band selector ---
        egui::CollapsingHeader::new(egui::RichText::new("Band").strong())
            .default_open(true)
            .show(ui, |ui| {
                let active_freq = self.app.rx(active_rx).map(|v| v.frequency_hz).unwrap_or(0);
                let current_band = Band::for_freq(active_freq);
                ui.columns(2, |cols| {
                    for (i, &band) in Band::ALL.iter().enumerate() {
                        let col = &mut cols[i % 2];
                        let is_current = current_band == Some(band);
                        let text = if is_current {
                            egui::RichText::new(band.label()).strong().color(Color32::BLACK)
                                .background_color(Color32::LIGHT_GREEN)
                        } else {
                            egui::RichText::new(band.label()).monospace()
                        };
                        if col.selectable_label(is_current, text).clicked() {
                            self.app.jump_to_band(band);
                        }
                    }
                });
            });

        ui.separator();

        // --- Filter presets + variable filter ---
        egui::CollapsingHeader::new(egui::RichText::new("Filter").strong())
            .default_open(true)
            .show(ui, |ui| {
                let state = self.app.rx(active_rx).cloned().unwrap_or_default();
                let bw = state.filter_hi - state.filter_lo;

                // Preset buttons in 2-column grid
                ui.columns(2, |cols| {
                    for (i, &preset) in FilterPreset::ALL.iter().enumerate() {
                        let col = &mut cols[i % 2];
                        let preset_bw = preset.width_hz();
                        let is_selected = (bw - preset_bw).abs() < 10.0;
                        let text = if is_selected {
                            egui::RichText::new(preset.label()).strong()
                                .color(Color32::BLACK)
                                .background_color(Color32::LIGHT_GREEN)
                        } else {
                            egui::RichText::new(preset.label()).monospace()
                        };
                        if col.selectable_label(is_selected, text).clicked() {
                            self.app.set_rx_filter_preset(rx_u8, preset);
                        }
                    }
                });

                ui.separator();

                // Variable filter: direct lo/hi spinners
                ui.label(egui::RichText::new("Variable:").small());
                ui.horizontal(|ui| {
                    ui.label("Lo:");
                    let mut lo = state.filter_lo;
                    if ui.add(egui::DragValue::new(&mut lo).speed(10.0).suffix(" Hz")).changed() {
                        self.app.set_rx_filter(rx_u8, lo, state.filter_hi);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Hi:");
                    let mut hi = state.filter_hi;
                    if ui.add(egui::DragValue::new(&mut hi).speed(10.0).suffix(" Hz")).changed() {
                        self.app.set_rx_filter(rx_u8, state.filter_lo, hi);
                    }
                });
                ui.label(format!("BW: {:.0} Hz", bw));
            });
    }

    /// Thin status bar at the very bottom: connection stats or
    /// "disconnected" label. Replaces the old inline connection
    /// status that was crammed into the top bar.
    fn draw_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.draw_connection_status(ui);
        });
    }

    /// Bottom controls panel. Shows a control row for the active RX,
    /// and when `num_rx == 2`, a second mirror row for RX2 beneath it.
    fn draw_bottom_panel(&mut self, ui: &mut egui::Ui) {
        let active = self.app.active_rx();
        self.draw_rx_controls(ui, active);

        // RX2 mirror row — Thetis upstream shows a dedicated 5-panel
        // strip for RX2 at the bottom of the console.
        if self.app.num_rx() >= 2 {
            let other = if active == 0 { 1 } else { 0 };
            ui.separator();
            self.draw_rx_controls(ui, other);
        }
    }

    /// One row of controls for a specific RX: Lock/Mute, AGC,
    /// NB/NB2/ANF/BIN/TNF toggles, display options, mode-specific
    /// panel. Used for both the active RX and the RX2 mirror row.
    fn draw_rx_controls(&mut self, ui: &mut egui::Ui, rx: usize) {
        let rx_u8 = rx as u8;
        let state = self.app.rx(rx).cloned().unwrap_or_default();
        let is_active = rx == self.app.active_rx();

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;

            // RX label — click to make this the active RX
            let label = format!("RX{}", rx + 1);
            let label_text = if is_active {
                egui::RichText::new(label).strong().color(Color32::LIGHT_GREEN)
            } else {
                egui::RichText::new(label).color(Color32::GRAY)
            };
            if ui.selectable_label(is_active, label_text).clicked() && !is_active {
                self.app.set_active_rx(rx);
            }

            ui.separator();

            // Lock / Mute
            ui.group(|ui| {
                let mut locked = state.locked;
                if ui.selectable_label(locked, if locked { "🔒" } else { "🔓" })
                    .on_hover_text("Lock VFO frequency")
                    .clicked()
                {
                    locked = !locked;
                    self.app.set_rx_locked(rx_u8, locked);
                }
                let mut muted = state.muted;
                if ui.selectable_label(muted, if muted { "🔇" } else { "🔊" })
                    .on_hover_text("Mute audio")
                    .clicked()
                {
                    muted = !muted;
                    self.app.set_rx_muted(rx_u8, muted);
                }
            });

            // AGC
            ui.group(|ui| {
                ui.label("AGC:");
                let mut agc = state.agc_mode;
                egui::ComboBox::from_id_salt(("agc", rx))
                    .selected_text(format!("{:?}", agc))
                    .width(55.0)
                    .show_ui(ui, |ui| {
                        use thetis_app::AgcPreset;
                        for m in [AgcPreset::Off, AgcPreset::Long, AgcPreset::Slow, AgcPreset::Med, AgcPreset::Fast] {
                            ui.selectable_value(&mut agc, m, format!("{m:?}"));
                        }
                    });
                if agc != state.agc_mode {
                    self.app.set_rx_agc(rx_u8, agc);
                }
            });

            // DSP toggles
            ui.group(|ui| {
                for (flag, label) in [
                    ("nb",  "NB"),
                    ("nb2", "NB2"),
                    ("anf", "ANF"),
                    ("bin", "BIN"),
                    ("tnf", "TNF"),
                ] {
                    let on = match flag {
                        "nb"  => state.nb,
                        "nb2" => state.nb2,
                        "anf" => state.anf,
                        "bin" => state.bin,
                        "tnf" => state.tnf,
                        _ => false,
                    };
                    let text = if on {
                        egui::RichText::new(label).color(Color32::BLACK)
                            .background_color(Color32::from_rgb(100, 200, 255))
                    } else {
                        egui::RichText::new(label).color(Color32::from_gray(120))
                    };
                    if ui.selectable_label(on, text)
                        .on_hover_text(match flag {
                            "nb"  => "Noise Blanker",
                            "nb2" => "Noise Blanker 2",
                            "anf" => "Auto Notch Filter",
                            "bin" => "Binaural audio",
                            "tnf" => "Tunable Notch Filter",
                            _ => "",
                        })
                        .clicked()
                    {
                        self.app.toggle_rx_flag(rx_u8, flag);
                    }
                }
            });

            // Mode-specific indicator
            ui.group(|ui| {
                match state.mode {
                    WdspMode::CwL | WdspMode::CwU => {
                        ui.label(egui::RichText::new("CW").strong());
                        ui.weak("Speed/Pitch — Phase C");
                    }
                    WdspMode::Fm => {
                        ui.label(egui::RichText::new("FM").strong());
                        ui.weak("Dev/CTCSS — Phase C");
                    }
                    WdspMode::DigL | WdspMode::DigU => {
                        ui.label(egui::RichText::new("DIG").strong());
                        ui.weak("VAC — Phase C");
                    }
                    _ => {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("PH").strong());
                            ui.weak("TX: Mic/VOX/CPDR — Phase C");
                        });
                    }
                }
            });
        });
    }

    /// Menu bar matching Thetis upstream: File / View / Help.
    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Quit").clicked() {
                    self.app.shutdown();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                for (kind, label) in [
                    (WindowKind::Memories,   "Memories"),
                    (WindowKind::BandStack,  "Band Stack"),
                    (WindowKind::Multimeter, "Multimeter"),
                    (WindowKind::Eq,         "Equalizer"),
                    (WindowKind::Repl,       "REPL"),
                    (WindowKind::Setup,      "Setup"),
                ] {
                    let open = self.app.window_open(kind);
                    if ui.selectable_label(open, label).clicked() {
                        self.app.toggle_window(kind);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label("Thetis-rs — Phase D");
                ui.hyperlink_to("Source", "https://github.com/jeff/thetis-rust");
            });
        });
    }

    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        // Row 1: global session controls (Connect / IP / num_rx)
        ui.horizontal(|ui| {
            if self.app.is_connected() {
                if ui.button("Disconnect").clicked() {
                    self.app.disconnect();
                }
            } else if ui.button("Connect").clicked() {
                self.app.connect();
            }

            ui.separator();
            ui.label("IP:");
            // Edit-buffer trick: we can't pass `&mut self.app.radio_ip()` because
            // App's getter returns &str. Use a local string mirror, and push
            // back to App via `set_radio_ip` if it changed.
            let mut ip_buf = self.app.radio_ip().to_string();
            let ip_resp = ui.add_enabled(
                !self.app.is_connected(),
                egui::TextEdit::singleline(&mut ip_buf).desired_width(120.0),
            );
            if ip_resp.changed() {
                self.app.set_radio_ip(ip_buf);
            }

            ui.separator();
            ui.label("RX:");
            // num_rx can only change while disconnected.
            let mut num_rx_buf = self.app.num_rx();
            ui.add_enabled_ui(!self.app.is_connected(), |ui| {
                ui.radio_value(&mut num_rx_buf, 1u8, "1");
                ui.radio_value(&mut num_rx_buf, 2u8, "2");
            });
            if num_rx_buf != self.app.num_rx() {
                self.app.set_num_rx(num_rx_buf);
            }

        });

        // Row 2+: one "VFO bar" per configured RX.
        for rx in 0..self.app.num_rx() as usize {
            ui.separator();
            self.draw_rx_row(ui, rx);
        }

        if let Some(e) = self.app.last_error() {
            ui.colored_label(Color32::LIGHT_RED, format!("error: {e}"));
        }
    }

    /// Floating "Memories" panel: scrollable list of named freq/mode
    /// bookmarks. Double-click a row to load it into the active RX,
    /// "Add" to capture the active RX's current state, "✕" to delete.
    fn draw_memories_window(&mut self, ctx: &egui::Context) {
        let mut open = self.app.window_open(WindowKind::Memories);
        let mut load_idx: Option<usize> = None;
        let mut delete_idx: Option<usize> = None;
        let mut add_clicked = false;
        let mem_count = self.app.memories().len();

        // Snapshot the memories list once for rendering, so we don't
        // alias `self.app` while we still need `self` for the form
        // input fields below.
        let memories: Vec<Memory> = self.app.memories().to_vec();

        egui::Window::new("Memories")
            .open(&mut open)
            .default_width(360.0)
            .default_height(380.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} memorie{}",
                    mem_count,
                    if mem_count == 1 { "" } else { "s" }
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
                    for (i, mem) in memories.iter().enumerate() {
                        ui.horizontal(|ui| {
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

        // Reflect window open/close back to App.
        self.app.set_window_open(WindowKind::Memories, open);

        if add_clicked {
            self.add_current_as_memory();
        }
        if let Some(i) = load_idx {
            self.app.load_memory(i);
        }
        if let Some(i) = delete_idx {
            self.app.delete_memory(i);
        }
    }

    /// Capture the active RX's current frequency + mode as a new memory.
    /// Uses the form-field name/tag, falling back to a "{freq:.3} MHz"
    /// auto-name when the user left the name blank.
    fn add_current_as_memory(&mut self) {
        let rx = self.app.active_rx();
        let Some(view) = self.app.rx(rx) else { return };
        let name = if self.new_memory_name.trim().is_empty() {
            format!("{:.3} MHz", view.frequency_hz as f64 / 1.0e6)
        } else {
            self.new_memory_name.trim().to_string()
        };
        let memory = Memory {
            name,
            freq_hz: view.frequency_hz,
            mode:    mode_to_serde(view.mode),
            tag:     self.new_memory_tag.trim().to_string(),
        };
        self.app.add_memory(memory);
        self.new_memory_name.clear();
        self.new_memory_tag.clear();
    }

    /// Floating band stack editor. Shows per-band freq + mode in a table;
    /// click a row to jump to that band.
    fn draw_band_stack_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Band Stack")
            .open(&mut open)
            .default_width(280.0)
            .default_height(320.0)
            .resizable(true)
            .show(ctx, |ui| {
                let active_rx = self.app.active_rx();
                let active_freq = self.app.rx(active_rx).map(|v| v.frequency_hz).unwrap_or(0);
                let current_band = Band::for_freq(active_freq);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("band-stack-grid")
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Band").strong());
                            ui.label(egui::RichText::new("Frequency").strong());
                            ui.label(egui::RichText::new("Mode").strong());
                            ui.end_row();

                            for band in Band::ALL {
                                let entry = self.app.band_stack().get(band);
                                let is_current = current_band == Some(band);
                                let label = if is_current {
                                    egui::RichText::new(band.label()).strong()
                                        .color(Color32::LIGHT_GREEN)
                                } else {
                                    egui::RichText::new(band.label()).monospace()
                                };
                                if ui.selectable_label(is_current, label).clicked() {
                                    self.app.jump_to_band(band);
                                }
                                ui.monospace(format!("{:>2}.{:03}.{:03}",
                                    entry.frequency_hz / 1_000_000,
                                    (entry.frequency_hz % 1_000_000) / 1_000,
                                    entry.frequency_hz % 1_000));
                                ui.monospace(format!("{:?}", entry.mode));
                                ui.end_row();
                            }
                        });
                });
            });
        if !open {
            self.app.set_window_open(WindowKind::BandStack, false);
        }
    }

    /// Floating multimeter: large S-meter bar with S-units, one per
    /// active RX. Bigger than the inline meter in the VFO row.
    fn draw_multimeter_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Multimeter")
            .open(&mut open)
            .default_width(350.0)
            .default_height(120.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(snapshot) = self.app.telemetry_snapshot() else {
                    ui.label("Not connected");
                    return;
                };
                let num_rx = snapshot.num_rx.min(MAX_RX as u8) as usize;
                for r in 0..num_rx {
                    let dbfs = snapshot.rx[r].s_meter_db;
                    let freq = self.app.rx(r).map(|s| s.frequency_hz).unwrap_or(0);
                    let cal_offset = Band::for_freq(freq)
                        .and_then(|b| self.app.calibration().smeter_offsets.get(b.label()))
                        .copied()
                        .unwrap_or(0.0);
                    let dbm  = dbfs - SMETER_DBFS_TO_DBM_OFFSET + cal_offset;
                    let s    = dbm_to_s_units(dbm);

                    ui.horizontal(|ui| {
                        ui.monospace(format!("RX{}", r + 1));

                        let bar_w = 200.0;
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(bar_w, 22.0),
                            Sense::hover(),
                        );
                        let painter = ui.painter();
                        painter.rect_filled(rect, 2.0, Color32::from_gray(20));

                        let s9_split = 0.6_f32;
                        let s_norm = if s <= 9.0 {
                            (s / 9.0) * s9_split
                        } else {
                            s9_split + ((s - 9.0) / 6.0).clamp(0.0, 1.0) * (1.0 - s9_split)
                        };
                        let filled = Rect::from_min_size(
                            rect.min,
                            Vec2::new(rect.width() * s_norm, rect.height()),
                        );
                        painter.rect_filled(filled, 2.0, level_color(dbfs));

                        // Tick marks
                        for i in 1..=9 {
                            let t = (i as f32 / 9.0) * s9_split;
                            let x = rect.min.x + t * rect.width();
                            painter.line_segment(
                                [Pos2::new(x, rect.max.y - 6.0), Pos2::new(x, rect.max.y)],
                                Stroke::new(1.0, Color32::from_gray(120)),
                            );
                        }

                        let readout = if s <= 9.0 {
                            format!("S{:.0}", s.round())
                        } else {
                            format!("S9+{:.0}", (dbm + 73.0).max(0.0))
                        };
                        ui.monospace(format!("{:<7} {:>6.1} dBm", readout, dbm));
                    });
                }
            });
        if !open {
            self.app.set_window_open(WindowKind::Multimeter, false);
        }
    }

    /// Floating 10-band graphic EQ window with vertical sliders.
    fn draw_eq_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let rx = self.app.active_rx() as u8;

        egui::Window::new("RX Equalizer")
            .open(&mut open)
            .default_width(480.0)
            .default_height(280.0)
            .resizable(true)
            .show(ctx, |ui| {
                let state = self.app.rx(rx as usize).cloned().unwrap_or_default();

                // Enable toggle
                let mut eq_on = state.eq_enabled;
                if ui.checkbox(&mut eq_on, "EQ Enabled").changed() {
                    self.app.set_rx_eq_enabled(rx, eq_on);
                }

                ui.separator();

                // Band labels
                let band_labels = [
                    "Pre", "32", "63", "125", "250", "500",
                    "1K", "2K", "4K", "8K", "16K",
                ];

                // Vertical sliders for each band
                ui.horizontal(|ui| {
                    for (i, &label) in band_labels.iter().enumerate() {
                        ui.vertical(|ui| {
                            let mut gain = state.eq_gains[i];
                            let resp = ui.add(
                                egui::Slider::new(&mut gain, -12..=12)
                                    .vertical()
                                    .show_value(false),
                            );
                            if resp.changed() {
                                self.app.set_rx_eq_band(rx, i, gain);
                            }
                            ui.monospace(format!("{gain:+}"));
                            ui.small(label);
                        });
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Flat").clicked() {
                        self.app.set_rx_eq_gains(rx, [0; 11]);
                    }
                    if ui.button("Bass Boost").clicked() {
                        self.app.set_rx_eq_gains(rx, [0, 8, 6, 4, 2, 0, 0, 0, 0, 0, 0]);
                    }
                    if ui.button("Treble Boost").clicked() {
                        self.app.set_rx_eq_gains(rx, [0, 0, 0, 0, 0, 0, 2, 4, 6, 8, 6]);
                    }
                    if ui.button("Voice").clicked() {
                        self.app.set_rx_eq_gains(rx, [0, -4, -2, 0, 2, 4, 4, 2, 0, -2, -4]);
                    }
                });
            });
        if !open {
            self.app.set_window_open(WindowKind::Eq, false);
        }
    }

    /// Floating REPL window: Rhai scripting console with a rich
    /// multi-line code editor (syntax highlighting, line numbers)
    /// and a scrollable color-coded output buffer.
    fn draw_repl_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("REPL")
            .open(&mut open)
            .default_width(600.0)
            .default_height(450.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Toolbar
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Script:").strong());
                    if ui.button("▶ Run (Ctrl+Enter)").clicked() {
                        self.run_repl_script();
                    }
                    if ui.button("Clear").clicked() {
                        self.script_engine.clear_output();
                    }
                });

                ui.separator();

                // Split the remaining space: output on top, editor
                // on bottom. Both are independently scrollable and
                // follow the window resize.
                let avail = ui.available_height();
                let output_h = (avail * 0.45).max(60.0);
                let editor_h = (avail - output_h - 8.0).max(60.0);

                // --- Output buffer ---
                egui::Frame::new()
                    .fill(Color32::from_rgb(10, 12, 16))
                    .inner_margin(egui::Margin::symmetric(4, 2))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("repl-output")
                            .max_height(output_h)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                if self.script_engine.output().is_empty() {
                                    ui.weak("Type Rhai code below, then Ctrl+Enter to run.");
                                    ui.weak("Example: freq(0, 14074000)");
                                }
                                for line in self.script_engine.output() {
                                    let color = match line.kind {
                                        ReplLineKind::Input  => Color32::from_gray(140),
                                        ReplLineKind::Result => Color32::from_rgb(100, 255, 120),
                                        ReplLineKind::Error  => Color32::from_rgb(255, 100, 100),
                                        ReplLineKind::Print  => Color32::from_rgb(100, 200, 255),
                                    };
                                    ui.monospace(egui::RichText::new(&line.text).color(color));
                                }
                            });
                    });

                ui.separator();

                // --- Code editor ---
                use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

                let syntax = Syntax::new("rhai")
                    .with_comment("//")
                    .with_comment_multiline(["/*", "*/"])
                    .with_keywords([
                        "let", "const", "fn", "if", "else", "while",
                        "for", "in", "loop", "break", "continue",
                        "return", "true", "false", "nil",
                    ])
                    .with_types([
                        "int", "float", "bool", "string", "char",
                        "Array", "Map",
                    ])
                    .with_special([
                        "freq", "mode", "volume", "nr3", "nr4",
                        "band", "do_connect", "do_disconnect",
                        "print", "rx0_freq", "rx0_mode",
                        "rx1_freq", "rx1_mode", "rx0_smeter",
                        "rx1_smeter", "active_rx", "connected",
                        "num_rx",
                    ]);

                egui::ScrollArea::vertical()
                    .id_salt("repl-editor-scroll")
                    .max_height(editor_h)
                    .show(ui, |ui| {
                        CodeEditor::default()
                            .id_source("repl-editor")
                            .with_rows(12)
                            .with_fontsize(14.0)
                            .with_theme(ColorTheme::GRUVBOX_DARK)
                            .with_syntax(syntax)
                            .with_numlines(true)
                            .show(ui, &mut self.repl_input);
                    });

                // Ctrl+Enter shortcut
                if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter)) {
                    self.run_repl_script();
                }
            });
        if !open {
            self.app.set_window_open(WindowKind::Repl, false);
        }
    }

    fn run_repl_script(&mut self) {
        let code = self.repl_input.clone();
        if code.trim().is_empty() {
            return;
        }
        self.script_engine.run_line(&code, &mut self.app);
        self.script_engine.apply_pending_commands(&mut self.app);
    }

    /// Floating Setup window with 5 tabs.
    fn draw_setup_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Setup")
            .open(&mut open)
            .default_width(500.0)
            .default_height(400.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Tab row
                ui.horizontal(|ui| {
                    for (i, label) in ["General", "Audio", "Display", "DSP", "Calibration"].iter().enumerate() {
                        if ui.selectable_label(self.setup_tab == i, *label).clicked() {
                            self.setup_tab = i;
                        }
                    }
                });
                ui.separator();

                match self.setup_tab {
                    0 => self.draw_setup_general(ui),
                    1 => self.draw_setup_audio(ui),
                    2 => self.draw_setup_display(ui),
                    3 => self.draw_setup_dsp(ui),
                    4 => self.draw_setup_calibration(ui),
                    _ => {}
                }
            });
        if !open {
            self.app.set_window_open(WindowKind::Setup, false);
        }
    }

    fn draw_setup_general(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("General").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Radio IP:");
            let mut ip = self.app.radio_ip().to_string();
            if ui.text_edit_singleline(&mut ip).changed() {
                self.app.set_radio_ip(ip);
            }
        });

        ui.horizontal(|ui| {
            ui.label("Default num_rx:");
            let mut n = self.app.num_rx();
            ui.radio_value(&mut n, 1u8, "1");
            ui.radio_value(&mut n, 2u8, "2");
            if n != self.app.num_rx() {
                self.app.set_num_rx(n);
            }
        });

        let mut auto_connect = self.app.display_settings().auto_connect;
        if ui.checkbox(&mut auto_connect, "Auto-connect on startup").changed() {
            self.app.display_settings_mut().auto_connect = auto_connect;
        }

        if let Some(path) = self.app.settings_path() {
            ui.add_space(8.0);
            ui.weak(format!("Config: {}", path.display()));
        }
    }

    fn draw_setup_audio(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Audio").strong());
        ui.add_space(4.0);

        let current = self.app.audio_device_name().to_string();
        let display = if current.is_empty() {
            "(system default)".to_string()
        } else {
            current.clone()
        };

        // Cache the device list so we only enumerate once per frame
        // (avoids the JACK/OSS stderr spam on every combo open).
        let devices = thetis_audio::enumerate_output_devices();

        ui.horizontal(|ui| {
            ui.label("Output device:");
            let mut changed_to: Option<String> = None;

            egui::ComboBox::from_id_salt("audio-device")
                .selected_text(&display)
                .width(300.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_empty(), "(system default)").clicked() {
                        changed_to = Some(String::new());
                    }
                    for name in &devices {
                        if ui.selectable_label(*name == current, name).clicked() {
                            changed_to = Some(name.clone());
                        }
                    }
                });

            if let Some(name) = changed_to {
                self.app.set_audio_device_name(name);
            }
        });

        ui.add_space(4.0);
        ui.weak("Changes take effect on next Connect.");

        // Show enumerated devices for reference
        ui.add_space(4.0);
        ui.collapsing("Available devices", |ui| {
            if devices.is_empty() {
                ui.weak("(no output devices found)");
            }
            for name in &devices {
                ui.monospace(name);
            }
        });
    }

    fn draw_setup_display(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Display").strong());
        ui.add_space(4.0);

        let ds = self.app.display_settings().clone();

        ui.horizontal(|ui| {
            ui.label("Spectrum min dB:");
            let mut min = ds.spectrum_min_db;
            if ui.add(egui::DragValue::new(&mut min).range(-160.0..=0.0).speed(1.0)).changed() {
                self.app.display_settings_mut().spectrum_min_db = min;
            }
        });

        ui.horizontal(|ui| {
            ui.label("Spectrum max dB:");
            let mut max = ds.spectrum_max_db;
            if ui.add(egui::DragValue::new(&mut max).range(-80.0..=20.0).speed(1.0)).changed() {
                self.app.display_settings_mut().spectrum_max_db = max;
            }
        });
    }

    fn draw_setup_dsp(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("DSP Defaults").strong());
        ui.add_space(4.0);

        let dsp = self.app.dsp_defaults().clone();

        ui.horizontal(|ui| {
            ui.label("Default AGC mode:");
            let mut agc = dsp.agc_mode.clone();
            egui::ComboBox::from_label("")
                .selected_text(&agc)
                .show_ui(ui, |ui| {
                    for m in ["Off", "Long", "Slow", "Med", "Fast"] {
                        ui.selectable_value(&mut agc, m.to_string(), m);
                    }
                });
            if agc != dsp.agc_mode {
                self.app.dsp_defaults_mut().agc_mode = agc;
            }
        });

        let mut nr3 = dsp.nr3_default;
        if ui.checkbox(&mut nr3, "NR3 on by default").changed() {
            self.app.dsp_defaults_mut().nr3_default = nr3;
        }

        let mut nr4 = dsp.nr4_default;
        if ui.checkbox(&mut nr4, "NR4 on by default").changed() {
            self.app.dsp_defaults_mut().nr4_default = nr4;
        }

        ui.horizontal(|ui| {
            ui.label("NR4 reduction (dB):");
            let mut red = dsp.nr4_reduction;
            if ui.add(egui::Slider::new(&mut red, 0.0..=40.0)).changed() {
                self.app.dsp_defaults_mut().nr4_reduction = red;
            }
        });
    }

    fn draw_setup_calibration(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("S-Meter Calibration").strong());
        ui.add_space(4.0);
        ui.weak("Offset per band (dBm). Adjusts the S-meter reading.");
        ui.add_space(4.0);

        let bands = [
            "160", "80", "60", "40", "30", "20", "17", "15", "12", "10", "6",
        ];

        egui::Grid::new("cal-grid").striped(true).show(ui, |ui| {
            ui.label(egui::RichText::new("Band").strong());
            ui.label(egui::RichText::new("Offset (dBm)").strong());
            ui.end_row();

            for band in bands {
                ui.label(format!("{band} m"));
                let current = self.app.calibration()
                    .smeter_offsets
                    .get(band)
                    .copied()
                    .unwrap_or(0.0);
                let mut val = current;
                if ui.add(egui::DragValue::new(&mut val).range(-20.0..=20.0).speed(0.1).suffix(" dB")).changed() {
                    self.app.calibration_mut()
                        .smeter_offsets
                        .insert(band.to_string(), val);
                }
                ui.end_row();
            }
        });
    }

    fn draw_rx_row(&mut self, ui: &mut egui::Ui, rx: usize) {
        let rx_u8 = rx as u8;
        let Some(state) = self.app.rx(rx).cloned() else { return };

        // --- Row 1: RX label + LED frequency + mode tag ---
        ui.horizontal(|ui| {
            // RX label with enable toggle
            let mut enabled = state.enabled;
            if ui.checkbox(&mut enabled, format!("RX{}", rx + 1)).changed() {
                self.app.set_rx_enabled(rx_u8, enabled);
            }

            ui.separator();

            // LED 7-segment frequency display using the DSEG7 Classic
            // font. Renders "14.074.500" in big green digits on a dark
            // background, matching the look of a hardware radio VFO
            // (Kenwood TS-2000 / Icom IC-7300 style). The DragValue
            // underneath provides click-to-edit and drag-to-tune.
            let freq = state.frequency_hz;
            let led_bg    = Color32::from_rgb(6, 10, 6);
            let led_color = Color32::from_rgb(80, 255, 100);
            let dseg_font = egui::FontId::new(
                32.0,
                egui::FontFamily::Name(FONT_DSEG7.into()),
            );

            egui::Frame::new()
                .fill(led_bg)
                .inner_margin(egui::Margin::symmetric(10, 4))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    // Override all text styles to DSEG7 + green for
                    // this scope. DragValue uses Body or Button style
                    // depending on whether it's being edited.
                    for style in [
                        egui::TextStyle::Body,
                        egui::TextStyle::Button,
                        egui::TextStyle::Monospace,
                    ] {
                        ui.style_mut().text_styles.insert(style, dseg_font.clone());
                    }
                    ui.visuals_mut().widgets.inactive.fg_stroke.color = led_color;
                    ui.visuals_mut().widgets.hovered.fg_stroke.color  = led_color;
                    ui.visuals_mut().widgets.active.fg_stroke.color   = led_color;
                    ui.visuals_mut().widgets.noninteractive.fg_stroke.color = led_color;
                    // Suppress the bg fill on the DragValue so only
                    // the dark Frame background shows through.
                    ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.hovered.bg_fill  = Color32::TRANSPARENT;

                    let mut freq_f = freq as f64;
                    let resp = ui.add(
                        egui::DragValue::new(&mut freq_f)
                            .range(0.0..=60_000_000.0)
                            .speed(10.0)
                            .custom_formatter(|v, _| {
                                let f = v as u32;
                                format!("{:>2}.{:03}.{:03}",
                                    f / 1_000_000,
                                    (f % 1_000_000) / 1_000,
                                    f % 1_000)
                            })
                            .custom_parser(|s| {
                                let clean: String = s.chars()
                                    .filter(|c| c.is_ascii_digit())
                                    .collect();
                                clean.parse::<f64>().ok()
                            }),
                    );
                    if resp.changed() {
                        self.app.set_rx_frequency(rx_u8, freq_f.max(0.0) as u32);
                    }
                });

            ui.separator();

            // Mode + band tag line
            let band_label = Band::for_freq(freq)
                .map(|b| b.label())
                .unwrap_or("GEN");
            ui.monospace(
                egui::RichText::new(format!("{}  {:?}", band_label, state.mode))
                    .color(Color32::from_rgb(220, 180, 60)),
            );
        });

        // --- Row 2: compact controls (volume + NR + status tags) ---
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            ui.label("AF");
            let mut vol_buf = state.volume;
            let vol_resp = ui.add(
                egui::Slider::new(&mut vol_buf, 0.0..=2.0)
                    .show_value(false),
            );
            if vol_resp.changed() {
                self.app.set_rx_volume(rx_u8, vol_buf);
            }

            ui.separator();

            // NR toggles as compact colored labels
            let mut nr3_buf = state.nr3;
            let nr3_text = if nr3_buf {
                egui::RichText::new("NR3").color(Color32::BLACK).background_color(Color32::LIGHT_GREEN)
            } else {
                egui::RichText::new("NR3").color(Color32::GRAY)
            };
            if ui.selectable_label(nr3_buf, nr3_text).clicked() {
                nr3_buf = !nr3_buf;
                self.app.set_rx_nr3(rx_u8, nr3_buf);
            }

            let mut nr4_buf = state.nr4;
            let nr4_text = if nr4_buf {
                egui::RichText::new("NR4").color(Color32::BLACK).background_color(Color32::LIGHT_GREEN)
            } else {
                egui::RichText::new("NR4").color(Color32::GRAY)
            };
            if ui.selectable_label(nr4_buf, nr4_text).clicked() {
                nr4_buf = !nr4_buf;
                self.app.set_rx_nr4(rx_u8, nr4_buf);
            }

            // Status tags (read-only indicators)
            ui.separator();
            ui.weak("AGC-MED");

            // Inline S-meter: compact bar + S-unit readout, Thetis-
            // style multimeter position (right side of VFO row).
            ui.separator();
            if let Some(snapshot) = self.app.telemetry_snapshot() {
                if rx < snapshot.rx.len() {
                    let dbfs = snapshot.rx[rx].s_meter_db;
                    let freq = self.app.rx(rx).map(|s| s.frequency_hz).unwrap_or(0);
                    let cal_offset = Band::for_freq(freq)
                        .and_then(|b| self.app.calibration().smeter_offsets.get(b.label()))
                        .copied()
                        .unwrap_or(0.0);
                    let dbm  = dbfs - SMETER_DBFS_TO_DBM_OFFSET + cal_offset;
                    let s    = dbm_to_s_units(dbm);

                    let bar_w = ui.available_width().clamp(60.0, 140.0);
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(bar_w, 14.0),
                        Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(rect, 2.0, Color32::from_gray(20));

                    let s9_split = 0.6_f32;
                    let s_norm = if s <= 9.0 {
                        (s / 9.0) * s9_split
                    } else {
                        s9_split + ((s - 9.0) / 6.0).clamp(0.0, 1.0) * (1.0 - s9_split)
                    };
                    let filled = Rect::from_min_size(
                        rect.min,
                        Vec2::new(rect.width() * s_norm, rect.height()),
                    );
                    painter.rect_filled(filled, 2.0, level_color(dbfs));

                    let readout = if s <= 9.0 {
                        format!("S{:.0}", s.round())
                    } else {
                        format!("S9+{:.0}", (dbm + 73.0).max(0.0))
                    };
                    ui.monospace(format!("{} {:+.0}", readout, dbm));
                }
            }
        });
    }

    fn draw_connection_status(&self, ui: &mut egui::Ui) {
        match self.app.radio() {
            Some(r) => {
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
            None => {
                ui.colored_label(Color32::GRAY, "○ disconnected");
            }
        }
    }

    fn draw_main(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(snapshot) = self.app.telemetry_snapshot() else {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("Not connected");
                ui.label(
                    "Set the radio IP + RX count in the top bar and click Connect.",
                );
            });
            return;
        };

        let num_rx = snapshot.num_rx.min(MAX_RX as u8) as usize;
        if num_rx == 0 {
            return;
        }

        // Push fresh rows into each RX's waterfall *before* drawing so
        // the visual and the data stay in sync. The waterfalls cache
        // is a frontend resource, owned by EguiView (not App).
        for r in 0..num_rx {
            self.waterfalls[r].push_row(&snapshot.rx[r].spectrum_bins_db);
            self.overlays[r].update(&snapshot.rx[r].spectrum_bins_db);
        }

        // Divide the central area into `num_rx` horizontal bands.
        // Each band gets its own spectrum on top and waterfall below.
        let avail = ui.available_size();
        let band_h = (avail.y / num_rx as f32).max(240.0);

        // Pending tune commands collected during the draw pass; applied
        // after the immutable telemetry borrow is released so we can
        // mutably touch self.app.
        let mut pending_tunes: Vec<(usize, u32)> = Vec::new();
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

                let ds = self.app.display_settings();
                draw_spectrum_ex(
                    ui, spec_rect,
                    &snapshot.rx[r].spectrum_bins_db,
                    ds.spectrum_min_db,
                    ds.spectrum_max_db,
                    true, // fill under curve
                );

                // Peak hold trace (white, 1px)
                if self.overlays[r].show_peak {
                    draw_trace_range(ui, spec_rect, &self.overlays[r].peak_bins,
                        Color32::from_rgba_premultiplied(255, 255, 200, 180),
                        ds.spectrum_min_db, ds.spectrum_max_db);
                }
                // Average trace (cyan, 1px)
                if self.overlays[r].show_avg {
                    draw_trace_range(ui, spec_rect, &self.overlays[r].avg_bins,
                        Color32::from_rgba_premultiplied(100, 200, 255, 160),
                        ds.spectrum_min_db, ds.spectrum_max_db);
                }

                // Passband overlay
                let rx_state = self.app.rx(r).cloned().unwrap_or_default();
                draw_passband_overlay(
                    ui, spec_rect,
                    snapshot.rx[r].span_hz,
                    rx_state.filter_lo as f32,
                    rx_state.filter_hi as f32,
                );

                // RX label
                let is_active = r == self.app.active_rx();
                let prefix = if is_active && num_rx > 1 { "▶ " } else { "" };
                ui.painter_at(spec_rect).text(
                    spec_rect.min + Vec2::new(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}RX{}  {:.3} MHz  {:?}",
                        prefix,
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

                self.waterfalls[r].draw(ui, ctx, water_rect);

                draw_vfo_marker(ui, spec_rect);
                draw_vfo_marker(ui, water_rect);

                let center_hz = snapshot.rx[r].center_freq_hz;
                let span_hz   = snapshot.rx[r].span_hz;

                // Single interact per rect — handles left-click tune
                // AND right-click context menu on the same Response.
                let spec_resp = ui.interact(
                    spec_rect,
                    egui::Id::new(("spec-tune", r)),
                    Sense::click_and_drag(),
                );
                spec_resp.context_menu(|ui| {
                    ui.checkbox(&mut self.overlays[r].show_peak, "Peak hold");
                    ui.checkbox(&mut self.overlays[r].show_avg,  "Average");
                });
                let (new_freq, clicked) = tune_from_response(
                    &spec_resp, ui, spec_rect, center_hz, span_hz,
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

        // Apply tune commands and active-RX promotion via App's
        // write API. The App handles mark_dirty + radio dispatch.
        for (rx, new_freq) in pending_tunes {
            self.app.set_rx_frequency(rx as u8, new_freq);
        }
        if let Some(rx) = newly_active {
            self.app.set_active_rx(rx);
        }
    }

}


// --- Theme ---------------------------------------------------------------

/// Tweak egui's stock dark visuals to give the radio more contrast
/// without shipping a custom font.
/// Custom font family name used for the 7-segment VFO display.
const FONT_DSEG7: &str = "DSEG7";

fn apply_dark_theme(ctx: &egui::Context) {
    // --- Register the DSEG7 7-segment font for VFO displays ---
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FONT_DSEG7.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(
            include_bytes!("../fonts/DSEG7Classic-Regular.ttf"),
        )),
    );
    fonts.families.insert(
        egui::FontFamily::Name(FONT_DSEG7.into()),
        vec![FONT_DSEG7.to_owned(), "Hack".to_owned()],
    );
    ctx.set_fonts(fonts);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = egui::Visuals::dark();

    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles.insert(TextStyle::Monospace, FontId::new(15.0, FontFamily::Monospace));
    style.text_styles.insert(TextStyle::Button,    FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body,      FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Heading,   FontId::new(20.0, FontFamily::Proportional));

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

    style.spacing.item_spacing       = Vec2::new(6.0, 4.0);
    style.spacing.button_padding     = Vec2::new(8.0, 3.0);
    style.spacing.interact_size      = Vec2::new(20.0, 22.0);

    ctx.set_global_style(style);
}

// --- Tuning interaction --------------------------------------------------

fn draw_vfo_marker(ui: &egui::Ui, rect: Rect) {
    let x = rect.center().x;
    ui.painter_at(rect).line_segment(
        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
        Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 160)),
    );
}

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

    if clicked {
        if let Some(pos) = response.interact_pointer_pos() {
            let dx_norm = (pos.x - rect.center().x) / rect.width();
            let delta_hz = (dx_norm * span_hz as f32).round() as i64;
            let next = (center_hz as i64 + delta_hz).max(0) as u32;
            if next != center_hz {
                new_freq = Some(next);
            }
        }
    }

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

/// Extract tune intent from an already-obtained Response (used when
/// the spectrum rect also needs a context_menu on the same Response).
fn tune_from_response(
    response: &egui::Response,
    ui: &egui::Ui,
    rect: Rect,
    center_hz: u32,
    span_hz: u32,
) -> (Option<u32>, bool) {
    let mut new_freq: Option<u32> = None;
    let clicked = response.clicked() || response.dragged();

    if clicked {
        if let Some(pos) = response.interact_pointer_pos() {
            let dx_norm = (pos.x - rect.center().x) / rect.width();
            let delta_hz = (dx_norm * span_hz as f32).round() as i64;
            let next = (center_hz as i64 + delta_hz).max(0) as u32;
            if next != center_hz {
                new_freq = Some(next);
            }
        }
    }

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

// --- Trace overlay (peak hold / average) ---------------------------------

fn draw_trace_range(ui: &egui::Ui, rect: Rect, bins: &[f32], color: Color32, min_db: f32, max_db: f32) {
    if bins.is_empty() {
        return;
    }
    let n = bins.len();
    let mut points = Vec::with_capacity(n);
    for (i, &db) in bins.iter().enumerate() {
        let x = rect.min.x + (i as f32 / (n - 1) as f32) * rect.width();
        let y = db_to_y_range(db, rect, min_db, max_db);
        points.push(Pos2::new(x, y));
    }
    ui.painter_at(rect).add(egui::Shape::line(
        points,
        Stroke::new(1.0, color),
    ));
}

// --- Passband overlay ----------------------------------------------------

fn draw_passband_overlay(ui: &egui::Ui, rect: Rect, span_hz: u32, f_lo: f32, f_hi: f32) {
    if span_hz == 0 || f_hi <= f_lo {
        return;
    }
    let span = span_hz as f32;
    let to_x = |hz: f32| -> f32 {
        let t = (hz / span) + 0.5;
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

fn draw_spectrum_ex(
    ui: &mut egui::Ui,
    rect: Rect,
    bins_db: &[f32],
    min_db: f32,
    max_db: f32,
    fill: bool,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(8, 10, 14));

    if bins_db.is_empty() {
        return;
    }

    // Grid lines every 20 dB within the visible range
    let grid_start = ((min_db / 20.0).ceil() as i32) * 20;
    let grid_end   = ((max_db / 20.0).floor() as i32) * 20;
    for db in (grid_start..=grid_end).step_by(20) {
        let y = db_to_y_range(db as f32, rect, min_db, max_db);
        let color = Color32::from_gray(48);
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, color),
        );
        // dB label on the left edge
        painter.text(
            Pos2::new(rect.min.x + 2.0, y - 6.0),
            egui::Align2::LEFT_TOP,
            format!("{db}"),
            egui::FontId::monospace(9.0),
            Color32::from_gray(80),
        );
    }

    // Spectrum polyline
    let n = bins_db.len();
    let mut points = Vec::with_capacity(n);
    for (i, &db) in bins_db.iter().enumerate() {
        let x = rect.min.x + (i as f32 / (n - 1) as f32) * rect.width();
        let y = db_to_y_range(db, rect, min_db, max_db);
        points.push(Pos2::new(x, y));
    }

    // Fill under the curve (Thetis PanFill)
    if fill && points.len() >= 2 {
        let mut fill_points = points.clone();
        fill_points.push(Pos2::new(rect.max.x, rect.max.y));
        fill_points.push(Pos2::new(rect.min.x, rect.max.y));
        painter.add(egui::Shape::convex_polygon(
            fill_points,
            Color32::from_rgba_premultiplied(40, 120, 60, 40),
            Stroke::NONE,
        ));
    }

    painter.add(egui::Shape::line(
        points,
        Stroke::new(1.6, Color32::from_rgb(140, 255, 160)),
    ));
}

fn db_to_y_range(db: f32, rect: Rect, min_db: f32, max_db: f32) -> f32 {
    let range = max_db - min_db;
    if range.abs() < 0.001 { return rect.max.y; }
    let clamped = db.clamp(min_db, max_db);
    let t = (clamped - min_db) / range;
    rect.max.y - t * rect.height()
}

fn level_color(db: f32) -> Color32 {
    if db < -80.0       { Color32::from_rgb(40, 120, 40) }
    else if db < -50.0  { Color32::from_rgb(80, 180, 80) }
    else if db < -20.0  { Color32::from_rgb(200, 200, 80) }
    else                { Color32::from_rgb(220, 80, 60) }
}

// --- Waterfall (egui-specific texture cache) ----------------------------

/// Per-RX scrolling waterfall display, owned by `EguiView` rather than
/// `App` because the `TextureHandle` is a frontend-specific resource.
struct Waterfall {
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

fn db_to_waterfall_color(db: f32) -> Color32 {
    let t = ((db + 120.0) / 120.0).clamp(0.0, 1.0);
    if t < 0.25 {
        let b = (t / 0.25 * 255.0) as u8;
        Color32::from_rgb(0, 0, b)
    } else if t < 0.5 {
        let g = ((t - 0.25) / 0.25 * 255.0) as u8;
        Color32::from_rgb(0, g, 255)
    } else if t < 0.75 {
        let r = ((t - 0.5) / 0.25 * 255.0) as u8;
        let b = 255 - r;
        Color32::from_rgb(r, 255, b)
    } else {
        let g = 255 - ((t - 0.75) / 0.25 * 255.0) as u8;
        Color32::from_rgb(255, g, 0)
    }
}
