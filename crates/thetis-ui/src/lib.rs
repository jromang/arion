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

impl RxView {
    fn new(freq: u32, mode: WdspMode, enabled: bool) -> Self {
        RxView {
            enabled,
            frequency_hz: freq,
            mode,
            volume: 0.25,
            nr3: false,
            nr4: false,
            waterfall: Waterfall::new(),
        }
    }
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
}

impl ThetisApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Seed the connection form from env vars so a one-liner
        // `HL2_IP=192.168.1.40 cargo run -p thetis` gets you straight
        // to a running receiver.
        let radio_ip = std::env::var("HL2_IP").unwrap_or_else(|_| "192.168.1.40".into());

        let mut rxs: Vec<RxView> = Vec::with_capacity(MAX_RX);
        // Seed two sensible defaults: 40m USB on RX1, 20m USB on RX2.
        rxs.push(RxView::new( 7_074_000, WdspMode::Usb, true));
        rxs.push(RxView::new(14_074_000, WdspMode::Usb, false));
        while rxs.len() < MAX_RX {
            rxs.push(RxView::new(7_074_000, WdspMode::Usb, false));
        }

        ThetisApp {
            radio:     None,
            telemetry: None,
            last_error: None,
            radio_ip,
            num_rx: 1,
            rxs,
        }
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
    }
}

impl eframe::App for ThetisApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep the UI animated even when the user isn't interacting —
        // the spectrum needs fresh draws at the DSP update rate (~23 Hz).
        ui.ctx().request_repaint_after(Duration::from_millis(40));

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
            ui.add_enabled(
                self.radio.is_none(),
                egui::TextEdit::singleline(&mut self.radio_ip).desired_width(120.0),
            );

            ui.separator();
            ui.label("RX:");
            // num_rx can only change while disconnected.
            ui.add_enabled_ui(self.radio.is_none(), |ui| {
                ui.radio_value(&mut self.num_rx, 1u8, "1");
                ui.radio_value(&mut self.num_rx, 2u8, "2");
            });

            ui.separator();
            self.draw_connection_status(ui);
        });

        // Row 2+: one "VFO bar" per configured RX.
        for rx in 0..self.num_rx as usize {
            ui.separator();
            self.draw_rx_row(ui, rx);
        }

        if let Some(e) = &self.last_error {
            ui.colored_label(Color32::LIGHT_RED, format!("error: {e}"));
        }
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
            }

            ui.separator();
            ui.label("Vol:");
            let prev_vol = self.rxs[rx].volume;
            ui.add(egui::Slider::new(&mut self.rxs[rx].volume, 0.0..=2.0).show_value(true));
            if (self.rxs[rx].volume - prev_vol).abs() > f32::EPSILON {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_volume(rx_u8, self.rxs[rx].volume);
                }
            }

            ui.separator();
            let prev_nr3 = self.rxs[rx].nr3;
            ui.checkbox(&mut self.rxs[rx].nr3, "NR3");
            if self.rxs[rx].nr3 != prev_nr3 {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_nr3(rx_u8, self.rxs[rx].nr3);
                }
            }
            let prev_nr4 = self.rxs[rx].nr4;
            ui.checkbox(&mut self.rxs[rx].nr4, "NR4");
            if self.rxs[rx].nr4 != prev_nr4 {
                if let Some(r) = &self.radio {
                    let _ = r.set_rx_nr4(rx_u8, self.rxs[rx].nr4);
                }
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
            }
        });
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
                ui.horizontal(|ui| {
                    ui.monospace(format!("RX{}", r + 1));
                    let db = snapshot.rx[r].s_meter_db;
                    // Map -100..0 dBFS to 0..1 fill.
                    let norm = ((db + 100.0) / 100.0).clamp(0.0, 1.0);

                    let bar_width = (ui.available_width() - 120.0).max(120.0);
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(bar_width, 14.0),
                        Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(rect, 2.0, Color32::from_gray(40));
                    let filled = Rect::from_min_size(
                        rect.min,
                        Vec2::new(rect.width() * norm, rect.height()),
                    );
                    painter.rect_filled(filled, 2.0, level_color(db));

                    ui.monospace(format!("{:>6.1} dBFS", db));
                });
            }
        });
    }
}

// --- Spectrum (immediate-mode line draw) --------------------------------

fn draw_spectrum(ui: &mut egui::Ui, rect: Rect, bins_db: &[f32]) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(12));

    if bins_db.is_empty() {
        return;
    }

    // Draw horizontal grid every 20 dB between -120 and 0.
    for db in (-120..=0).step_by(20) {
        let y = db_to_y(db as f32, rect);
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, Color32::from_gray(40)),
        );
    }

    // Map FFT bins horizontally across the rect and plot as a polyline.
    let n = bins_db.len();
    let mut points = Vec::with_capacity(n);
    for (i, &db) in bins_db.iter().enumerate() {
        let x = rect.min.x + (i as f32 / (n - 1) as f32) * rect.width();
        let y = db_to_y(db, rect);
        points.push(Pos2::new(x, y));
    }
    painter.add(egui::Shape::line(points, Stroke::new(1.5, Color32::LIGHT_GREEN)));
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
