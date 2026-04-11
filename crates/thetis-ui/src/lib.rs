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
use thetis_core::{Radio, RadioConfig, Telemetry, WdspMode, SPECTRUM_BINS};

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

/// Top-level eframe app state.
pub struct ThetisApp {
    // --- Live radio handle (None = disconnected) --------------------
    radio:     Option<Radio>,
    telemetry: Option<Arc<ArcSwap<Telemetry>>>,
    last_error: Option<String>,

    // --- UI state / form fields ------------------------------------
    radio_ip:     String,
    frequency_hz: u32,
    mode:         WdspMode,
    volume:       f32,

    // --- Waterfall renderer ----------------------------------------
    waterfall: Waterfall,
}

impl ThetisApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // Seed the connection form from env vars so a one-liner
        // `HL2_IP=192.168.1.40 cargo run -p thetis` gets you straight
        // to a running receiver.
        let radio_ip = std::env::var("HL2_IP").unwrap_or_else(|_| "192.168.1.40".into());

        ThetisApp {
            radio:     None,
            telemetry: None,
            last_error: None,
            radio_ip,
            frequency_hz: 7_074_000,
            mode:         WdspMode::Usb,
            volume:       0.25,
            waterfall:    Waterfall::new(),
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

        let config = RadioConfig {
            radio_addr:    addr,
            rx1_frequency: self.frequency_hz,
            mode:          self.mode,
            volume:        self.volume,
            audio_device:  None,
        };

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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the UI animated even when the user isn't interacting —
        // the spectrum needs fresh draws at the DSP update rate (~23 Hz).
        ctx.request_repaint_after(Duration::from_millis(40));

        egui::TopBottomPanel::top("top-bar").show(ctx, |ui| {
            self.draw_top_bar(ui);
        });

        egui::TopBottomPanel::bottom("s-meter").show(ctx, |ui| {
            self.draw_s_meter(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_main(ui, ctx);
        });
    }
}

// --- UI sub-sections ----------------------------------------------------

impl ThetisApp {
    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
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
            ui.label("VFO:");
            let mut freq = self.frequency_hz as f64;
            let changed = ui
                .add(
                    egui::DragValue::new(&mut freq)
                        .range(0.0..=60_000_000.0)
                        .speed(10.0)
                        .suffix(" Hz"),
                )
                .changed();
            if changed {
                self.frequency_hz = freq.max(0.0) as u32;
                if let Some(r) = &self.radio {
                    let _ = r.set_frequency(self.frequency_hz);
                }
            }
            ui.label(format!("({:.3} MHz)", self.frequency_hz as f64 / 1.0e6));

            ui.separator();
            ui.label("Mode:");
            let prev_mode = self.mode;
            egui::ComboBox::from_id_salt("mode")
                .selected_text(format!("{:?}", self.mode))
                .show_ui(ui, |ui| {
                    for m in [
                        WdspMode::Lsb, WdspMode::Usb, WdspMode::Am, WdspMode::Sam,
                        WdspMode::Fm, WdspMode::CwL, WdspMode::CwU,
                        WdspMode::DigL, WdspMode::DigU,
                    ] {
                        ui.selectable_value(&mut self.mode, m, format!("{m:?}"));
                    }
                });
            if self.mode != prev_mode {
                if let Some(r) = &self.radio {
                    let _ = r.set_mode(self.mode);
                }
            }

            ui.separator();
            ui.label("Vol:");
            let prev_vol = self.volume;
            ui.add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(true));
            if (self.volume - prev_vol).abs() > f32::EPSILON {
                if let Some(r) = &self.radio {
                    let _ = r.set_volume(self.volume);
                }
            }

            ui.separator();
            self.draw_connection_status(ui);
        });

        if let Some(e) = &self.last_error {
            ui.colored_label(Color32::LIGHT_RED, format!("error: {e}"));
        }
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
                    "Set the radio IP in the top bar and click Connect.",
                );
            });
            return;
        };

        let snapshot = telem.load_full();

        // Feed the newest row into the waterfall before drawing.
        self.waterfall.push_row(&snapshot.spectrum_bins_db);

        // Split the central area 1/3 spectrum, 2/3 waterfall.
        let avail = ui.available_size();
        let spec_h  = (avail.y * 0.35).max(120.0);
        let water_h = (avail.y - spec_h - 6.0).max(120.0);

        ui.vertical(|ui| {
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(avail.x, spec_h),
                Sense::hover(),
            );
            draw_spectrum(ui, rect, &snapshot.spectrum_bins_db);

            ui.add_space(4.0);

            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(avail.x, water_h),
                Sense::hover(),
            );
            self.waterfall.draw(ui, ctx, rect);
        });
    }

    fn draw_s_meter(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let db = match &self.telemetry {
                Some(t) => t.load_full().s_meter_db,
                None    => -140.0,
            };
            // Map -100..0 dBFS to 0..1 fill.
            let norm = ((db + 100.0) / 100.0).clamp(0.0, 1.0);

            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width() - 120.0, 18.0),
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
