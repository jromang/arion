//! egui + wgpu desktop frontend for Arion.
//!
//! This crate is a **humble view** in the MVVM split. The application's
//! state, event handling, persistence, and radio interactions all live
//! in [`arion_app::App`]. This crate's job is exclusively:
//!
//! 1. Read from `&App` to render egui widgets
//! 2. Translate user gestures (clicks, drags, scrolls) into
//!    `App::set_*` / `App::toggle_*` calls
//! 3. Cache the per-RX waterfall textures (egui-specific resource)
//! 4. Own the eframe entry point + apply our dark theme
//!
//! No application logic should live here. If you find yourself wanting
//! to add a `mark_dirty` or push a `DspCommand` from this crate, the
//! method belongs in `arion-app` instead.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};

use arion_app::{
    dbm_to_s_units, mode_to_serde, AgcPreset, App, AppOptions, Band, FilterPreset, WindowKind,
    SMETER_DBFS_TO_DBM_OFFSET,
};
use arion_settings::WaterfallPalette;
use arion_core::{WdspMode, MAX_RX, SPECTRUM_BINS};
use arion_script::{FnHandle, ReplLineKind, ScriptEngine};

mod bandplan;
mod script_ui;
use arion_settings::Memory;

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
            .with_title("Arion"),
        ..Default::default()
    };

    eframe::run_native(
        "Arion",
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
    /// Selected RX index inside the Setup → DSP per-RX fine-tuning panel.
    setup_dsp_rx: usize,
    /// Rhai scripting engine + REPL state.
    script_engine: ScriptEngine,
    /// Script editor tabs (one per open file or scratch buffer).
    script_tabs: Vec<ScriptTab>,
    /// Currently active script tab index.
    active_script_tab: usize,
    /// rigctld request sender (cloned to session threads).
    rigctld_tx:     mpsc::Sender<arion_rigctld::RigRequest>,
    /// rigctld request receiver (drained every frame).
    rigctld_rx:     mpsc::Receiver<arion_rigctld::RigRequest>,
    /// Active rigctld server handle, if running.
    rigctld_handle: Option<arion_rigctld::RigctldHandle>,
    /// Last-known rigctld status string for the Setup UI.
    rigctld_status: String,
    /// MIDI action sender — cloned per listener restart.
    midi_action_tx: mpsc::Sender<arion_midi::MidiAction>,
    /// MIDI action receiver (drained every frame).
    midi_action_rx: mpsc::Receiver<arion_midi::MidiAction>,
    /// MIDI raw-event sender — cloned per listener restart.
    midi_event_tx:  mpsc::Sender<arion_midi::MidiEvent>,
    /// MIDI raw-event receiver (drained into `midi_last_event` each frame).
    midi_event_rx:  mpsc::Receiver<arion_midi::MidiEvent>,
    /// Hot-swappable mapping shared with the midir callback thread.
    midi_mapping:   arion_midi::SharedMapping,
    /// Active MIDI listener handle. Dropping it closes the port.
    midi_listener:  Option<arion_midi::MidiListener>,
    /// Last raw event seen — displayed in the Setup MIDI tab.
    midi_last_event: Option<arion_midi::MidiEvent>,
    /// Status string for the Setup MIDI tab.
    midi_status:    String,
    /// Devices enumerated on the last refresh.
    midi_devices:   Vec<String>,
    /// Web-frontend state snapshot — republished each frame and
    /// read by the web server thread to push JSON to browsers.
    web_snapshot:     arion_web::SharedSnapshot,
    /// Telemetry mirror for the web server. Updated each frame from
    /// `app.telemetry_snapshot()` (if a radio is live).
    web_telemetry:    arion_web::SharedTelemetry,
    /// Receives actions dispatched from browser clients.
    web_action_rx:    mpsc::Receiver<arion_web::Action>,
    /// arion-api action channel (shared with any external HTTP client).
    api_action_tx:    mpsc::Sender<arion_app::protocol::Action>,
    api_action_rx:    mpsc::Receiver<arion_app::protocol::Action>,
    /// arion-api script eval queue (UI thread runs Rhai on demand).
    api_script_tx:    mpsc::Sender<arion_api::ScriptRequest>,
    api_script_rx:    mpsc::Receiver<arion_api::ScriptRequest>,
    /// Shared ArcSwap for the last MIDI event (used by REST /midi/last-event).
    api_midi_last_event: std::sync::Arc<arc_swap::ArcSwap<Option<arion_midi::MidiEvent>>>,
    /// Active API server handle. Dropping it shuts the server down.
    api_handle:       Option<arion_api::ApiHandle>,
    /// Human-readable status for the Setup API tab.
    api_status:       String,
    /// Pre-allocated audio-tap producer, attached to the live `Radio`
    /// on the first successful `connect()`. `None` once attached, or
    /// if the tap was never instantiated.
    web_audio_producer: Option<rtrb::Producer<arion_core::StereoFrame>>,
    /// Per-RX zoom/pan state for the spectrum display.
    view_states: Vec<RxViewState>,
    /// Per-RX passband drag state (which edge/center is being dragged).
    passband_drags: Vec<PassbandDrag>,
    /// Per-RX display layout (Panafall / Spectrum / Waterfall / Split).
    display_modes: Vec<DisplayMode>,
    /// Per-RX split fraction in Split mode — height fraction for the spectrum ∈ [0.15, 0.85].
    split_fractions: Vec<f32>,
}

/// One script editor tab: a buffer that may or may not be backed by
/// a file on disk. `dirty` tracks whether the in-memory content has
/// diverged from the last saved/loaded state.
#[derive(Debug, Clone)]
struct ScriptTab {
    name: String,
    path: Option<PathBuf>,
    content: String,
    dirty: bool,
}

impl ScriptTab {
    fn scratch(n: usize) -> Self {
        ScriptTab {
            name: format!("scratch-{n}.rhai"),
            path: None,
            content: String::new(),
            dirty: false,
        }
    }

    fn from_path(path: PathBuf, content: String) -> Self {
        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("untitled.rhai")
            .to_string();
        ScriptTab { name, path: Some(path), content, dirty: false }
    }

    /// Short tab label with a • marker when dirty.
    fn label(&self) -> String {
        if self.dirty {
            format!("● {}", self.name)
        } else {
            self.name.clone()
        }
    }
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

        let waterfalls: Vec<_> = (0..MAX_RX).map(Waterfall::new).collect();
        let overlays        = (0..MAX_RX).map(|_| SpectrumOverlay::new()).collect();
        let view_states     = (0..MAX_RX).map(|_| RxViewState::default()).collect();
        let passband_drags  = (0..MAX_RX).map(|_| PassbandDrag::None).collect();
        let display_modes   = (0..MAX_RX).map(|_| DisplayMode::default()).collect();
        let split_fractions = (0..MAX_RX).map(|_| 0.5_f32).collect();

        let (rigctld_tx, rigctld_rx) = mpsc::channel::<arion_rigctld::RigRequest>();
        let (api_action_tx, api_action_rx) = mpsc::channel::<arion_app::protocol::Action>();
        let (api_script_tx, api_script_rx) = mpsc::channel::<arion_api::ScriptRequest>();
        let api_midi_last_event: std::sync::Arc<arc_swap::ArcSwap<Option<arion_midi::MidiEvent>>> =
            std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(None)));

        // MIDI bridge: an optional listener pushes resolved actions on
        // `midi_action_tx`; the UI thread drains it each frame. The
        // mapping is behind an ArcSwap so the Setup tab can swap
        // bindings without restarting the backend thread.
        let (midi_action_tx, midi_action_rx) = mpsc::channel::<arion_midi::MidiAction>();
        let (midi_event_tx, midi_event_rx) = mpsc::channel::<arion_midi::MidiEvent>();
        let persisted = arion_midi::persist::load();
        let initial_mapping = if persisted.bindings.is_empty() {
            arion_midi::default_mapping()
        } else {
            persisted
        };
        let midi_mapping = arion_midi::shared(initial_mapping);
        let midi_devices = arion_midi::device::enum_inputs().unwrap_or_default();

        // Web bridge: snapshot + action channel + telemetry mirror +
        // audio tap. The server runs in its own thread with its own
        // tokio runtime. Gated behind `ARION_WEB_LISTEN=<addr>` — if
        // the env var is absent, the web frontend is disabled and no
        // thread is spawned.
        let web_snapshot: arion_web::SharedSnapshot =
            std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                arion_web::StateSnapshot::default(),
            )));
        let web_telemetry: arion_web::SharedTelemetry =
            std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
                arion_core::Telemetry::default(),
            )));
        let (web_action_tx, web_action_rx) = mpsc::channel::<arion_web::Action>();
        let (web_audio_producer, web_audio_consumer) =
            rtrb::RingBuffer::<arion_core::StereoFrame>::new(48_000 / 2); // ~500 ms buffer
        let web_audio_tap: arion_web::SharedAudioTap =
            std::sync::Arc::new(std::sync::Mutex::new(Some(web_audio_consumer)));
        if let Ok(addr_str) = std::env::var("ARION_WEB_LISTEN") {
            match addr_str.parse::<std::net::SocketAddr>() {
                Ok(addr) => {
                    let snap = web_snapshot.clone();
                    let tel = web_telemetry.clone();
                    let tap = web_audio_tap.clone();
                    std::thread::Builder::new()
                        .name("arion-web".into())
                        .spawn(move || {
                            if let Err(e) =
                                arion_web::serve_blocking(addr, snap, web_action_tx, tel, tap)
                            {
                                tracing::warn!(error = %e, "arion-web server exited");
                            }
                        })
                        .ok();
                }
                Err(e) => tracing::warn!(error = %e, "ARION_WEB_LISTEN invalid, web disabled"),
            }
        }

        let mut view = EguiView {
            app,
            waterfalls,
            overlays,
            view_states,
            passband_drags,
            display_modes,
            split_fractions,
            new_memory_name:    String::new(),
            new_memory_tag:     String::new(),
            setup_tab:          0,
            setup_dsp_rx:       0,
            script_engine:      ScriptEngine::default(),
            script_tabs:        vec![ScriptTab::scratch(1)],
            active_script_tab:  0,
            rigctld_tx,
            rigctld_rx,
            rigctld_handle:     None,
            rigctld_status:     "stopped".into(),
            midi_action_tx,
            midi_action_rx,
            midi_event_tx,
            midi_event_rx,
            midi_mapping,
            midi_listener:      None,
            midi_last_event:    None,
            midi_status:        "stopped".into(),
            midi_devices,
            api_action_tx,
            api_action_rx,
            api_script_tx,
            api_script_rx,
            api_midi_last_event,
            api_handle: None,
            api_status: "stopped".into(),
            web_snapshot,
            web_telemetry,
            web_action_rx,
            web_audio_producer: Some(web_audio_producer),
        };
        view.load_startup_script();
        // Auto-start rigctld if enabled in settings.
        if view.app.network_settings().rigctld_enabled {
            view.start_rigctld();
        }
        if view.app.network_settings().api_enabled {
            view.start_api();
        }
        // Auto-start MIDI if enabled in settings, or if the legacy
        // ARION_MIDI_DEVICE env var is set (kept for CI / scripted runs).
        if let Ok(needle) = std::env::var("ARION_MIDI_DEVICE") {
            view.app.midi_settings_mut().enabled = true;
            view.app.midi_settings_mut().device_name = Some(needle);
        }
        if view.app.midi_settings().enabled {
            view.start_midi();
        }
        view
    }

    /// Load and run `~/.config/arion/startup.rhai` if it exists.
    /// Silent when the file is absent. I/O or evaluation errors log
    /// a warning and are pushed as a REPL error line so the user can
    /// see them when they open the Scripts window.
    fn load_startup_script(&mut self) {
        let Some(path) = arion_script::startup_script_path() else {
            return;
        };
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                let msg = format!("startup.rhai: read error: {e}");
                tracing::warn!(path = %path.display(), "{msg}");
                self.script_engine.push_output(ReplLineKind::Error, msg);
                return;
            }
        };
        tracing::info!(path = %path.display(), "loading startup script");
        if let Err(e) = self.script_engine.run_script(&source, &mut self.app) {
            tracing::warn!(error = %e, "startup.rhai evaluation failed");
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

        // Drain rigctld requests arriving from session threads.
        arion_rigctld::drain(&mut self.app, &self.rigctld_rx);
        // Drain REST API actions.
        for _ in 0..64 {
            match self.api_action_rx.try_recv() {
                Ok(a) => a.apply(&mut self.app),
                Err(_) => break,
            }
        }
        // Drain REST API script requests (limit one per frame to
        // avoid starving the UI if a client spams /scripts/eval).
        if let Ok(req) = self.api_script_rx.try_recv() {
            let reply = self.evaluate_api_script(&req.source);
            let _ = req.reply.send(reply);
        }
        // Reconcile API server lifecycle against settings.
        let want_api = self.app.network_settings().api_enabled;
        let have_api = self.api_handle.is_some();
        if want_api && !have_api {
            self.start_api();
        } else if !want_api && have_api {
            self.stop_api();
        }
        // Reconcile rigctld server lifecycle against settings.
        let want_rig = self.app.network_settings().rigctld_enabled;
        let have_rig = self.rigctld_handle.is_some();
        if want_rig && !have_rig {
            self.start_rigctld();
        } else if !want_rig && have_rig {
            self.stop_rigctld();
        }
        // Reconcile MIDI listener against settings.
        let want_midi = self.app.midi_settings().enabled;
        let have_midi = self.midi_listener.is_some();
        if want_midi && !have_midi {
            self.start_midi();
        } else if !want_midi && have_midi {
            self.stop_midi();
        }
        // Drain MIDI actions arriving from the midir callback thread.
        arion_midi::drain(&mut self.app, &self.midi_action_rx);
        // Drain raw MIDI events into the Setup-tab "last event" slot.
        while let Ok(ev) = self.midi_event_rx.try_recv() {
            self.midi_last_event = Some(ev);
            self.api_midi_last_event.store(std::sync::Arc::new(Some(ev)));
        }

        // Drain web actions from browser clients.
        while let Ok(action) = self.web_action_rx.try_recv() {
            action.apply(&mut self.app);
        }

        // Attach the audio tap to the live radio on the first frame
        // after connect. Consumed once — disconnect+reconnect won't
        // re-attach; restart arion if you need audio again over the
        // web after a disconnect.
        if self.web_audio_producer.is_some() && self.app.radio().is_some() {
            let producer = self.web_audio_producer.take().unwrap();
            if let Some(radio) = self.app.radio() {
                if let Err(e) = radio.set_audio_tap(Some(producer)) {
                    tracing::warn!(error = %e, "failed to attach web audio tap");
                }
            }
        }

        // Publish snapshot + telemetry for the web server.
        if let Some(tel) = self.app.telemetry_snapshot() {
            self.web_telemetry.store(tel.clone());
            let snap = arion_web::StateSnapshot::from_app_and_telemetry(&self.app, &tel);
            self.web_snapshot.store(std::sync::Arc::new(snap));
        } else {
            let tel = arion_core::Telemetry::default();
            let snap = arion_web::StateSnapshot::from_app_and_telemetry(&self.app, &tel);
            self.web_snapshot.store(std::sync::Arc::new(snap));
        }

        // --- Arion-style panel layout (D.1) ---
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
        if self.app.window_open(WindowKind::Digital) {
            self.draw_digital_window(&ctx);
        }
        if self.app.window_open(WindowKind::Repl) {
            self.draw_repl_window(&ctx);
        }
        if self.app.window_open(WindowKind::Setup) {
            self.draw_setup_window(&ctx);
        }

        // Scripted windows (declared from Rhai via `window(id, title, ||…)`).
        script_ui::render_script_ui(&ctx, &mut self.script_engine, &mut self.app);
    }

    /// Final flush on window close. eframe calls this exactly once
    /// after the user closes the viewport, so it's the right place
    /// to disconnect the radio cleanly and persist the last state.
    fn on_exit(&mut self) {
        if let Some(h) = self.rigctld_handle.take() {
            h.stop();
        }
        self.app.shutdown();
    }
}

// --- UI sub-sections ----------------------------------------------------

impl EguiView {
    /// Right side panel: Mode selector + Band selector + Filter presets.
    /// Replaces the old in-row ComboBox mode picker and the inline band
    /// buttons — these now live in their own dedicated right-hand column
    /// matching the Arion upstream layout.
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

        // RX2 mirror row — Arion upstream shows a dedicated 5-panel
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
                        use arion_app::AgcPreset;
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
                for (flag, label, tip) in [
                    ("nb",      "NB",   "Noise Blanker"),
                    ("nb2",     "NB2",  "Noise Blanker 2"),
                    ("anr",     "ANR",  "LMS Adaptive NR (Thetis \"NR\")"),
                    ("emnr",    "EMNR", "Enhanced Spectral NR (Thetis \"NR2\")"),
                    ("anf",     "ANF",  "Auto Notch Filter"),
                    ("squelch", "SQL",  "Squelch (FM/AM/SSB auto)"),
                    ("apf",     "APF",  "Audio Peak Filter (CW)"),
                    ("bin",     "BIN",  "Binaural audio"),
                    ("tnf",     "TNF",  "Tunable Notch Filter"),
                ] {
                    let on = match flag {
                        "nb"      => state.nb,
                        "nb2"     => state.nb2,
                        "anr"     => state.anr,
                        "emnr"    => state.emnr,
                        "anf"     => state.anf,
                        "squelch" => state.squelch,
                        "apf"     => state.apf,
                        "bin"     => state.bin,
                        "tnf"     => state.tnf,
                        _ => false,
                    };
                    let text = if on {
                        egui::RichText::new(label).color(Color32::BLACK)
                            .background_color(Color32::from_rgb(100, 200, 255))
                    } else {
                        egui::RichText::new(label).color(Color32::from_gray(120))
                    };
                    if ui.selectable_label(on, text)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        // squelch and apf have dedicated setters; the rest
                        // flip through toggle_rx_flag.
                        match flag {
                            "squelch" => self.app.set_rx_squelch(rx_u8, !on),
                            "apf"     => self.app.set_rx_apf(rx_u8, !on),
                            _         => self.app.toggle_rx_flag(rx_u8, flag),
                        }
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

    /// Menu bar matching Arion upstream: File / View / Help.
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
                    (WindowKind::Digital,    "Digital Decodes"),
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
                ui.label("Arion — Phase D");
                ui.hyperlink_to("Source", "https://github.com/jeff/arion");
            });

            // Scripts menu: populated dynamically from `menu_item(path, fn)`.
            let items: Vec<(String, FnHandle)> = {
                let ui_state = self.script_engine.ui_state();
                let s = ui_state.borrow();
                s.menu_items.clone()
            };
            if !items.is_empty() {
                let mut to_dispatch: Vec<FnHandle> = Vec::new();
                ui.menu_button("Scripts", |ui| {
                    for (path, handle) in &items {
                        let label = path.rsplit('/').next().unwrap_or(path.as_str());
                        if ui.button(label).clicked() {
                            to_dispatch.push(handle.clone());
                            ui.close();
                        }
                    }
                });
                for h in to_dispatch {
                    let _ = self.script_engine.dispatch_callback(&h, &mut self.app);
                }
            }
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
    fn draw_digital_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let rx = self.app.active_rx() as u8;
        let current = self.app.rx_digital_mode(rx);
        let decodes = self.app.rx_digital_decodes(rx);

        egui::Window::new("Digital Decodes")
            .open(&mut open)
            .default_width(420.0)
            .default_height(320.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Mode:");
                    let label = match current {
                        None => "Off",
                        Some(arion_core::DigitalMode::Psk31) => "PSK31",
                        Some(arion_core::DigitalMode::Psk63) => "PSK63",
                        Some(arion_core::DigitalMode::Rtty) => "RTTY",
                        Some(arion_core::DigitalMode::Aprs) => "APRS",
                    };
                    egui::ComboBox::from_id_salt("digital_mode_combo")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for (m, lbl) in [
                                (None, "Off"),
                                (Some(arion_core::DigitalMode::Psk31), "PSK31"),
                                (Some(arion_core::DigitalMode::Psk63), "PSK63"),
                                (Some(arion_core::DigitalMode::Rtty), "RTTY"),
                                (Some(arion_core::DigitalMode::Aprs), "APRS"),
                            ] {
                                if ui.selectable_label(current == m, lbl).clicked() {
                                    self.app.set_rx_digital_mode(rx, m);
                                }
                            }
                        });
                });
                ui.separator();
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    if decodes.is_empty() {
                        ui.weak("(no decodes yet — decoder pipeline pending)");
                    } else {
                        for d in decodes {
                            ui.monospace(format!("[{} {:+.0} dB] {}", d.mode.as_str(), d.snr_db, d.text));
                        }
                    }
                });
            });
        if !open {
            self.app.set_window_open(WindowKind::Digital, false);
        }
    }

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
        egui::Window::new("Script Editor")
            .open(&mut open)
            .default_width(900.0)
            .default_height(650.0)
            .min_width(500.0)
            .min_height(400.0)
            .resizable(true)
            .show(ctx, |ui| {
                // --- Top toolbar: file operations + run ---
                self.draw_script_toolbar(ui);
                ui.separator();

                // --- Tab bar ---
                self.draw_script_tab_bar(ui);
                ui.separator();

                // --- Main split: editor (top ~70%) + output (bottom ~30%) ---
                let avail = ui.available_height();
                let output_h = (avail * 0.30).clamp(80.0, 400.0);
                let editor_h = (avail - output_h - 8.0).max(120.0);

                self.draw_script_editor(ui, editor_h);
                ui.separator();
                self.draw_script_output(ui, output_h);

                // Global shortcuts while window has focus
                ui.input(|i| {
                    if i.modifiers.ctrl && i.key_pressed(egui::Key::Enter) {
                        ctx.memory_mut(|m| m.data.insert_temp(
                            egui::Id::new("run-script-req"), true));
                    }
                    if i.modifiers.ctrl && i.key_pressed(egui::Key::S) {
                        ctx.memory_mut(|m| m.data.insert_temp(
                            egui::Id::new("save-script-req"), true));
                    }
                    if i.modifiers.ctrl && i.key_pressed(egui::Key::N) {
                        ctx.memory_mut(|m| m.data.insert_temp(
                            egui::Id::new("new-script-req"), true));
                    }
                });
            });

        // Consume any keyboard shortcut requests raised above
        if ctx.memory(|m| m.data.get_temp::<bool>(egui::Id::new("run-script-req")).unwrap_or(false)) {
            ctx.memory_mut(|m| m.data.remove::<bool>(egui::Id::new("run-script-req")));
            self.run_current_script();
        }
        if ctx.memory(|m| m.data.get_temp::<bool>(egui::Id::new("save-script-req")).unwrap_or(false)) {
            ctx.memory_mut(|m| m.data.remove::<bool>(egui::Id::new("save-script-req")));
            self.save_current_tab();
        }
        if ctx.memory(|m| m.data.get_temp::<bool>(egui::Id::new("new-script-req")).unwrap_or(false)) {
            ctx.memory_mut(|m| m.data.remove::<bool>(egui::Id::new("new-script-req")));
            self.new_tab();
        }

        if !open {
            self.app.set_window_open(WindowKind::Repl, false);
        }
    }

    fn draw_script_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📄 New").on_hover_text("New tab (Ctrl+N)").clicked() {
                self.new_tab();
            }
            if ui.button("📂 Open…").on_hover_text("Open file").clicked() {
                self.open_file_dialog();
            }
            if ui.button("💾 Save").on_hover_text("Save (Ctrl+S)").clicked() {
                self.save_current_tab();
            }
            if ui.button("💾 Save As…").on_hover_text("Save As…").clicked() {
                self.save_as_current_tab();
            }
            ui.separator();
            if ui.button(egui::RichText::new("▶ Run").strong().color(Color32::LIGHT_GREEN))
                .on_hover_text("Run current script (Ctrl+Enter)")
                .clicked()
            {
                self.run_current_script();
            }
            if ui.button("Clear Output").clicked() {
                self.script_engine.clear_output();
            }
            // Right-aligned status
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(tab) = self.script_tabs.get(self.active_script_tab) {
                    if let Some(path) = &tab.path {
                        ui.weak(format!("{}", path.display()));
                    } else {
                        ui.weak("(unsaved)");
                    }
                }
            });
        });
    }

    fn draw_script_tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close_idx: Option<usize> = None;
        egui::ScrollArea::horizontal()
            .id_salt("script-tab-bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.script_tabs.iter().enumerate() {
                        let selected = i == self.active_script_tab;
                        let text = egui::RichText::new(tab.label())
                            .color(if selected { Color32::WHITE } else { Color32::from_gray(160) });
                        if ui.selectable_label(selected, text).clicked() {
                            self.active_script_tab = i;
                        }
                        // Small close button per tab
                        if self.script_tabs.len() > 1
                            && ui.small_button("✕").on_hover_text("Close tab").clicked()
                        {
                            close_idx = Some(i);
                        }
                        ui.separator();
                    }
                });
            });
        if let Some(i) = close_idx {
            self.close_tab(i);
        }
    }

    fn draw_script_editor(&mut self, ui: &mut egui::Ui, height: f32) {
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
                "Array", "Map", "Radio", "Rx",
            ])
            .with_special([
                // Root object
                "radio",
                // Rx property setters / getters (accessed via radio[i].prop)
                "freq", "mode", "volume", "muted", "locked", "enabled",
                "filter_lo", "filter_hi", "nr3", "nr4", "agc",
                "nb", "nb2", "anf", "bin", "tnf",
                "eq_enabled", "eq_gains",
                "s_meter", "spectrum", "center_freq",
                // Free-function action API
                "filter", "filter_preset", "eq", "eq_band",
                "band", "tune", "mute", "lock",
                "active_rx", "num_rx", "connect", "disconnect",
                "memory_save", "memory_load", "memory_delete",
                "window", "save", "audio_device", "help",
                // Scripted UI builders
                "window_show", "window_hide", "window_toggle",
                "on_change", "menu_item",
                "label", "button", "slider", "checkbox", "text_edit",
                "separator", "hbox", "vbox",
                // Misc
                "print",
            ]);

        let Some(tab) = self.script_tabs.get_mut(self.active_script_tab) else {
            return;
        };
        let before = tab.content.clone();

        egui::ScrollArea::vertical()
            .id_salt("script-editor-scroll")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_height(height);
                CodeEditor::default()
                    .id_source("script-editor")
                    .with_rows(30)
                    .with_fontsize(14.0)
                    .with_theme(ColorTheme::GRUVBOX_DARK)
                    .with_syntax(syntax)
                    .with_numlines(true)
                    .show(ui, &mut tab.content);
            });

        if tab.content != before {
            tab.dirty = true;
        }
    }

    fn draw_script_output(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.label(egui::RichText::new("Output").strong());
        egui::Frame::new()
            .fill(Color32::from_rgb(10, 12, 16))
            .inner_margin(egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("script-output")
                    .max_height(height - 22.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_height(height - 22.0);
                        if self.script_engine.output().is_empty() {
                            ui.weak("Run a script with ▶ Run or Ctrl+Enter.");
                            ui.weak("Example: freq(0, 14074000)  //  tune RX0 to 14.074 MHz");
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
    }

    fn run_current_script(&mut self) {
        let code = self.script_tabs.get(self.active_script_tab)
            .map(|t| t.content.clone())
            .unwrap_or_default();
        if code.trim().is_empty() {
            return;
        }
        self.script_engine.run_line(&code, &mut self.app);
    }

    fn new_tab(&mut self) {
        let n = self.script_tabs.len() + 1;
        self.script_tabs.push(ScriptTab::scratch(n));
        self.active_script_tab = self.script_tabs.len() - 1;
    }

    fn close_tab(&mut self, idx: usize) {
        if self.script_tabs.len() <= 1 || idx >= self.script_tabs.len() {
            return;
        }
        self.script_tabs.remove(idx);
        if self.active_script_tab >= self.script_tabs.len() {
            self.active_script_tab = self.script_tabs.len() - 1;
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Rhai scripts", &["rhai"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    self.script_tabs.push(ScriptTab::from_path(path, content));
                    self.active_script_tab = self.script_tabs.len() - 1;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to read script file");
                }
            }
        }
    }

    fn save_current_tab(&mut self) {
        let Some(tab) = self.script_tabs.get(self.active_script_tab) else { return };
        match tab.path.clone() {
            Some(path) => {
                let content = tab.content.clone();
                match std::fs::write(&path, content) {
                    Ok(()) => {
                        if let Some(t) = self.script_tabs.get_mut(self.active_script_tab) {
                            t.dirty = false;
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to save script"),
                }
            }
            None => self.save_as_current_tab(),
        }
    }

    fn save_as_current_tab(&mut self) {
        let Some(tab) = self.script_tabs.get(self.active_script_tab).cloned() else { return };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Rhai scripts", &["rhai"])
            .set_file_name(&tab.name)
            .save_file()
        {
            match std::fs::write(&path, &tab.content) {
                Ok(()) => {
                    if let Some(t) = self.script_tabs.get_mut(self.active_script_tab) {
                        t.path = Some(path.clone());
                        t.name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("untitled.rhai")
                            .to_string();
                        t.dirty = false;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to save script"),
            }
        }
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
                    for (i, label) in ["General", "Audio", "Display", "DSP", "Calibration", "Network", "MIDI"].iter().enumerate() {
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
                    5 => self.draw_setup_network(ui),
                    6 => self.draw_setup_midi(ui),
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
        let devices = arion_audio::enumerate_output_devices();

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

        ui.horizontal(|ui| {
            ui.label("Waterfall palette:");
            let mut palette = ds.waterfall_palette;
            egui::ComboBox::from_id_salt("wf_palette")
                .selected_text(palette.label())
                .show_ui(ui, |ui| {
                    for &p in WaterfallPalette::ALL {
                        ui.selectable_value(&mut palette, p, p.label());
                    }
                });
            if palette != ds.waterfall_palette {
                self.app.display_settings_mut().waterfall_palette = palette;
            }
        });

        ui.horizontal(|ui| {
            ui.label("Bandplan region:");
            let mut region = ds.bandplan_region;
            egui::ComboBox::from_id_salt("bandplan_region")
                .selected_text(region.label())
                .show_ui(ui, |ui| {
                    for &r in arion_settings::BandplanRegion::ALL {
                        ui.selectable_value(&mut region, r, r.label());
                    }
                });
            if region != ds.bandplan_region {
                self.app.display_settings_mut().bandplan_region = region;
            }
        });

        ui.horizontal(|ui| {
            ui.label("Waterfall speed:");
            let mut speed = ds.waterfall_speed;
            if ui.add(egui::Slider::new(&mut speed, 1..=8)
                .text("frames/row")).changed()
            {
                self.app.display_settings_mut().waterfall_speed = speed;
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

        ui.add_space(10.0);
        ui.separator();
        ui.label(egui::RichText::new("Per-RX fine tuning").strong());
        ui.weak("Changes apply immediately and are persisted per RX.");
        ui.add_space(4.0);

        // Per-RX selector — only one receiver's knobs are displayed at a
        // time to keep the panel compact.
        let num_rx = self.app.num_rx() as usize;
        let selected = self.setup_dsp_rx.min(num_rx.saturating_sub(1));
        ui.horizontal(|ui| {
            ui.label("Receiver:");
            for r in 0..num_rx {
                let label = format!("RX{}", r + 1);
                if ui.selectable_label(selected == r, label).clicked() {
                    self.setup_dsp_rx = r;
                }
            }
        });
        let rx_idx = self.setup_dsp_rx.min(num_rx.saturating_sub(1));
        let Some(state) = self.app.rx(rx_idx).cloned() else { return };
        let rx = rx_idx as u8;

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Squelch")
            .default_open(true)
            .show(ui, |ui| {
                let mut on = state.squelch;
                if ui.checkbox(&mut on, "Enable squelch (mode-dispatched)").changed() {
                    self.app.set_rx_squelch(rx, on);
                }
                let mut th = state.squelch_db;
                ui.horizontal(|ui| {
                    ui.label("Threshold:");
                    if ui.add(egui::Slider::new(&mut th, -100.0..=0.0).suffix(" dB"))
                        .changed()
                    {
                        self.app.set_rx_squelch_threshold(rx, th);
                    }
                });
            });

        egui::CollapsingHeader::new("APF — CW audio peak filter")
            .show(ui, |ui| {
                let mut on = state.apf;
                if ui.checkbox(&mut on, "Enable APF").changed() {
                    self.app.set_rx_apf(rx, on);
                }
                ui.horizontal(|ui| {
                    ui.label("Centre:");
                    let mut f = state.apf_freq_hz;
                    if ui.add(egui::Slider::new(&mut f, 200.0..=1500.0).suffix(" Hz"))
                        .changed() { self.app.set_rx_apf_freq(rx, f); }
                });
                ui.horizontal(|ui| {
                    ui.label("Bandwidth:");
                    let mut b = state.apf_bw_hz;
                    if ui.add(egui::Slider::new(&mut b, 10.0..=500.0).suffix(" Hz"))
                        .changed() { self.app.set_rx_apf_bandwidth(rx, b); }
                });
                ui.horizontal(|ui| {
                    ui.label("Gain:");
                    let mut g = state.apf_gain_db;
                    if ui.add(egui::Slider::new(&mut g, 0.0..=20.0).suffix(" dB"))
                        .changed() { self.app.set_rx_apf_gain(rx, g); }
                });
            });

        egui::CollapsingHeader::new("AGC fine tuning")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Top level:");
                    let mut v = state.agc_top_dbm;
                    if ui.add(egui::Slider::new(&mut v, -120.0..=0.0).suffix(" dBm"))
                        .changed() { self.app.set_rx_agc_top(rx, v); }
                });
                ui.horizontal(|ui| {
                    ui.label("Hang level:");
                    let mut v = state.agc_hang_level;
                    if ui.add(egui::Slider::new(&mut v, -100.0..=0.0).suffix(" dB"))
                        .changed() { self.app.set_rx_agc_hang_level(rx, v); }
                });
                ui.horizontal(|ui| {
                    ui.label("Decay:");
                    let mut v = state.agc_decay_ms;
                    if ui.add(egui::Slider::new(&mut v, 50..=5000).suffix(" ms"))
                        .changed() { self.app.set_rx_agc_decay(rx, v); }
                });
                ui.horizontal(|ui| {
                    ui.label("Fixed gain (when AGC Off):");
                    let mut v = state.agc_fixed_gain;
                    if ui.add(egui::Slider::new(&mut v, 0.0..=100.0).suffix(" dB"))
                        .changed() { self.app.set_rx_agc_fixed_gain(rx, v); }
                });
            });

        if matches!(state.mode, WdspMode::Fm) {
            egui::CollapsingHeader::new("FM parameters")
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Deviation:");
                        let mut narrow = state.fm_deviation_hz <= 3000.0;
                        if ui.selectable_label(narrow, "Narrow (2.5 kHz)").clicked() {
                            self.app.set_rx_fm_deviation(rx, 2500.0);
                            narrow = true;
                        }
                        if ui.selectable_label(!narrow, "Wide (5 kHz)").clicked() {
                            self.app.set_rx_fm_deviation(rx, 5000.0);
                        }
                    });
                    let mut ct_on = state.ctcss_on;
                    if ui.checkbox(&mut ct_on, "CTCSS tone squelch").changed() {
                        self.app.set_rx_ctcss(rx, ct_on);
                    }
                    ui.horizontal(|ui| {
                        ui.label("CTCSS freq:");
                        let mut hz = state.ctcss_hz;
                        if ui.add(egui::Slider::new(&mut hz, 67.0..=254.1).suffix(" Hz"))
                            .changed() { self.app.set_rx_ctcss_freq(rx, hz); }
                    });
                });
        }

        if matches!(state.mode, WdspMode::Sam) {
            egui::CollapsingHeader::new("SAM sub-mode")
                .default_open(true)
                .show(ui, |ui| {
                    let labels = [(0u8, "DSB"), (1, "LSB"), (2, "USB")];
                    ui.horizontal(|ui| {
                        for (val, lbl) in labels {
                            if ui.selectable_label(state.sam_submode == val, lbl).clicked() {
                                self.app.set_rx_sam_submode(rx, val);
                            }
                        }
                    });
                });
        }
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

    fn draw_setup_network(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Network — rigctld server").strong());
        ui.add_space(4.0);
        ui.weak("Expose a Hamlib-compatible TCP server so WSJT-X, fldigi, GPredict, etc. can drive Arion.");
        ui.add_space(6.0);

        let net = self.app.network_settings().clone();

        let mut enabled = net.rigctld_enabled;
        if ui.checkbox(&mut enabled, "Enable rigctld server").changed() {
            self.app.network_settings_mut().rigctld_enabled = enabled;
            if enabled {
                self.start_rigctld();
            } else {
                self.stop_rigctld();
            }
        }

        ui.horizontal(|ui| {
            ui.label("Port:");
            let mut port = net.rigctld_port as u32;
            let running = self.rigctld_handle.is_some();
            let resp = ui.add_enabled(
                !running,
                egui::DragValue::new(&mut port).range(1u32..=65535u32).speed(1.0),
            );
            if resp.changed() {
                self.app.network_settings_mut().rigctld_port = port.clamp(1, 65535) as u16;
            }
            if running {
                ui.weak("(stop the server to edit)");
            }
        });

        ui.add_space(6.0);
        ui.label(format!("Status: {}", self.rigctld_status));

        ui.add_space(12.0);
        ui.separator();
        ui.label(egui::RichText::new("REST API server").strong());
        ui.add_space(4.0);
        ui.weak(
            "HTTP/JSON control surface under /api/v1/* — consumable from curl, Python, Home \
             Assistant, Prometheus, etc. Localhost binding only by default.",
        );
        ui.add_space(6.0);

        let net2 = self.app.network_settings().clone();
        let api_running = self.api_handle.is_some();

        let mut api_enabled = net2.api_enabled;
        if ui.checkbox(&mut api_enabled, "Enable REST API").changed() {
            self.app.network_settings_mut().api_enabled = api_enabled;
        }

        ui.horizontal(|ui| {
            ui.label("Port:");
            let mut port = net2.api_port as u32;
            let resp = ui.add_enabled(
                !api_running,
                egui::DragValue::new(&mut port).range(1u32..=65535u32).speed(1.0),
            );
            if resp.changed() {
                self.app.network_settings_mut().api_port = port.clamp(1, 65535) as u16;
            }
            if api_running {
                ui.weak("(stop to edit)");
            }
        });

        let mut loopback = net2.api_bind_loopback;
        if ui.add_enabled(!api_running, egui::Checkbox::new(&mut loopback, "Bind to loopback only (127.0.0.1)")).changed() {
            self.app.network_settings_mut().api_bind_loopback = loopback;
        }
        if !loopback {
            ui.colored_label(egui::Color32::YELLOW,
                "⚠ Non-loopback binding with no auth is unsafe on shared networks.");
        }

        let mut allow_scripts = net2.api_allow_scripts;
        if ui.checkbox(&mut allow_scripts, "Allow /scripts/eval (Rhai execution)").changed() {
            self.app.network_settings_mut().api_allow_scripts = allow_scripts;
        }
        if allow_scripts {
            ui.colored_label(egui::Color32::YELLOW,
                "⚠ Scripts have full access to the radio state. Keep loopback only.");
        }

        ui.add_space(6.0);
        ui.label(format!("Status: {}", self.api_status));
    }

    /// Spawn the rigctld server on the configured port. Updates
    /// `rigctld_status` with a human-readable message. No-op if the
    /// server is already running.
    fn start_rigctld(&mut self) {
        if self.rigctld_handle.is_some() {
            return;
        }
        let port = self.app.network_settings().rigctld_port;
        let addr: std::net::SocketAddr = match format!("127.0.0.1:{port}").parse() {
            Ok(a) => a,
            Err(e) => {
                self.rigctld_status = format!("invalid address: {e}");
                return;
            }
        };
        match arion_rigctld::RigctldHandle::start(addr, self.rigctld_tx.clone()) {
            Ok(h) => {
                self.rigctld_status = format!("running on {}", h.addr());
                self.rigctld_handle = Some(h);
            }
            Err(e) => {
                self.rigctld_status = format!("failed: {e}");
                tracing::warn!(error = %e, "rigctld start failed");
            }
        }
    }

    fn stop_rigctld(&mut self) {
        if let Some(h) = self.rigctld_handle.take() {
            h.stop();
            self.rigctld_status = "stopped".into();
        }
    }

    /// Spawn the REST API on the configured port. No-op if already running.
    fn start_api(&mut self) {
        if self.api_handle.is_some() {
            return;
        }
        let net = self.app.network_settings().clone();
        let host: std::net::IpAddr = if net.api_bind_loopback {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        };
        let addr = std::net::SocketAddr::new(host, net.api_port);
        let ctx = arion_api::ApiContext {
            snapshot:       self.web_snapshot.clone(),
            telemetry:      self.web_telemetry.clone(),
            action_tx:      self.api_action_tx.clone(),
            script_tx:      net.api_allow_scripts.then(|| self.api_script_tx.clone()),
            midi_mapping:   Some(self.midi_mapping.clone()),
            midi_last_event: self.api_midi_last_event.clone(),
            midi_persist:   true,
            midi_persist_path: arion_midi::persist::midi_config_path(),
            started_at:     std::time::Instant::now(),
            build_version:  env!("CARGO_PKG_VERSION"),
        };
        match arion_api::start(addr, ctx) {
            Ok(h) => {
                self.api_status = format!("running on {}", h.addr());
                self.api_handle = Some(h);
            }
            Err(e) => {
                self.api_status = format!("failed: {e}");
                tracing::warn!(error = %e, "arion-api start failed");
            }
        }
    }

    fn stop_api(&mut self) {
        if let Some(h) = self.api_handle.take() {
            h.stop();
            self.api_status = "stopped".into();
        }
    }

    /// Evaluate a Rhai source fragment submitted via the REST API.
    /// Runs on the UI thread so it has full access to `App` via the
    /// scripting engine. Errors are captured and returned in the
    /// reply instead of panicking the server.
    fn evaluate_api_script(&mut self, source: &str) -> arion_api::ScriptReply {
        use arion_script::engine::ReplLineKind;
        let before = self.script_engine.output().len();
        self.script_engine.run_line(source, &mut self.app);
        let after = self.script_engine.output();
        let mut output = String::new();
        let mut error: Option<String> = None;
        for line in &after[before..] {
            match line.kind {
                ReplLineKind::Error => error = Some(line.text.clone()),
                ReplLineKind::Result | ReplLineKind::Print => {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&line.text);
                }
                _ => {}
            }
        }
        arion_api::ScriptReply { output, error }
    }

    fn start_midi(&mut self) {
        if self.midi_listener.is_some() {
            return;
        }
        let Some(needle) = self.app.midi_settings().device_name.clone() else {
            self.midi_status = "no device selected".into();
            return;
        };
        match arion_midi::start(
            &needle,
            self.midi_mapping.clone(),
            self.midi_action_tx.clone(),
            self.midi_event_tx.clone(),
        ) {
            Ok(l) => {
                self.midi_status = format!("connected to {}", l.port_name());
                self.midi_listener = Some(l);
            }
            Err(e) => {
                self.midi_status = format!("failed: {e}");
                tracing::warn!(error = %e, needle = %needle, "midi start failed");
            }
        }
    }

    fn stop_midi(&mut self) {
        if self.midi_listener.take().is_some() {
            self.midi_status = "stopped".into();
        }
    }

    fn refresh_midi_devices(&mut self) {
        match arion_midi::device::enum_inputs() {
            Ok(v) => self.midi_devices = v,
            Err(e) => tracing::warn!(error = %e, "midi enum failed"),
        }
    }

    fn draw_setup_midi(&mut self, ui: &mut egui::Ui) {
        use arion_midi::Trigger;

        ui.label(egui::RichText::new("MIDI controller").strong());
        ui.add_space(4.0);
        ui.weak("Map MIDI CC / Note messages to Arion actions. Encoder 'Relative' scale works for endless encoders using the Mackie 2's-complement convention (1..63 = CW, 65..127 = CCW).");
        ui.add_space(6.0);

        let midi = self.app.midi_settings().clone();

        let mut enabled = midi.enabled;
        if ui.checkbox(&mut enabled, "Enable MIDI input").changed() {
            self.app.midi_settings_mut().enabled = enabled;
            if enabled {
                self.start_midi();
            } else {
                self.stop_midi();
            }
        }

        ui.horizontal(|ui| {
            ui.label("Device:");
            let running = self.midi_listener.is_some();
            let mut selected = midi.device_name.clone().unwrap_or_default();
            let resp = egui::ComboBox::from_id_salt("midi-device")
                .selected_text(if selected.is_empty() {
                    "(pick a device)".to_string()
                } else {
                    selected.clone()
                })
                .show_ui(ui, |ui| {
                    let mut changed = false;
                    for name in &self.midi_devices {
                        if ui.selectable_value(&mut selected, name.clone(), name).changed() {
                            changed = true;
                        }
                    }
                    changed
                });
            let changed = resp.inner.unwrap_or(false);
            if changed {
                self.app.midi_settings_mut().device_name = Some(selected);
                if running {
                    self.stop_midi();
                    self.start_midi();
                }
            }
            if ui.button("Rescan").clicked() {
                self.refresh_midi_devices();
            }
        });

        ui.add_space(6.0);
        ui.label(format!("Status: {}", self.midi_status));

        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Last event (learn)").strong());
        match self.midi_last_event {
            Some(arion_midi::MidiEvent { trigger: Trigger::Cc { channel, controller }, value }) => {
                ui.monospace(format!("CC  ch={channel:<2} ctrl={controller:<3} val={value}"));
            }
            Some(arion_midi::MidiEvent { trigger: Trigger::Note { channel, note }, value }) => {
                ui.monospace(format!("Note ch={channel:<2} note={note:<3} vel={value}"));
            }
            None => {
                ui.weak("(move a control on your MIDI device)");
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Bindings").strong());
        let snap = self.midi_mapping.load_full();
        egui::Grid::new("midi-bindings").striped(true).show(ui, |ui| {
            ui.label(egui::RichText::new("Trigger").strong());
            ui.label(egui::RichText::new("Scale").strong());
            ui.label(egui::RichText::new("Target").strong());
            ui.end_row();
            for b in &snap.bindings {
                let trig = match b.trigger {
                    Trigger::Cc { channel, controller } => format!("CC ch={channel} ctrl={controller}"),
                    Trigger::Note { channel, note } => format!("Note ch={channel} n={note}"),
                };
                let scale = match b.scale {
                    arion_midi::Scale::Absolute { min, max } => format!("Abs [{min}, {max}]"),
                    arion_midi::Scale::Relative { step } => format!("Rel step={step}"),
                    arion_midi::Scale::Trigger => "Trigger".into(),
                };
                ui.monospace(trig);
                ui.monospace(scale);
                ui.monospace(format!("{:?}", b.target));
                ui.end_row();
            }
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Reset to default mapping").clicked() {
                self.midi_mapping.store(std::sync::Arc::new(arion_midi::default_mapping()));
                self.save_midi_mapping();
            }
            if ui.button("Save mapping").clicked() {
                self.save_midi_mapping();
            }
        });
        ui.weak(format!(
            "Mapping file: {}",
            arion_midi::persist::midi_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(no config dir)".into())
        ));
    }

    fn save_midi_mapping(&self) {
        let table = self.midi_mapping.load_full();
        match arion_midi::persist::save(&table) {
            Ok(()) => tracing::info!("midi: mapping saved"),
            Err(e) => tracing::warn!(error = %e, "midi: save failed"),
        }
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

            // Inline S-meter: compact bar + S-unit readout, Arion-
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
        let ds_pre = self.app.display_settings();
        for r in 0..num_rx {
            self.waterfalls[r].push_row(
                &snapshot.rx[r].spectrum_bins_db,
                ds_pre.spectrum_min_db,
                ds_pre.spectrum_max_db,
                ds_pre.waterfall_palette,
                ds_pre.waterfall_speed,
            );
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

                let n_bins = snapshot.rx[r].spectrum_bins_db.len().max(1);
                let (lo_bin, hi_bin) = self.view_states[r].visible_bins(n_bins);
                let center_hz = snapshot.rx[r].center_freq_hz;
                let span_hz   = snapshot.rx[r].span_hz;

                // Compute visible Hz window (absolute)
                let lo_hz = center_hz as f32 - span_hz as f32 * 0.5
                    + lo_bin as f32 / n_bins as f32 * span_hz as f32;
                let hi_hz = center_hz as f32 - span_hz as f32 * 0.5
                    + hi_bin as f32 / n_bins as f32 * span_hz as f32;

                // UV coords for waterfall cropping [0, 1]
                let uv_lo = lo_bin as f32 / n_bins as f32;
                let uv_hi = hi_bin as f32 / n_bins as f32;

                // Layout: depends on DisplayMode.
                // Panafall: spec 35% + axis + waterfall; Split: same but draggable divider;
                // Spectrum: full-band spec + axis only; Waterfall: axis + full-band waterfall.
                let axis_h  = 18.0_f32;
                let gap     = 4.0_f32;
                let mode    = self.display_modes[r];
                let (show_spec, show_water) = match mode {
                    DisplayMode::Panafall  | DisplayMode::Split => (true, true),
                    DisplayMode::Spectrum  => (true,  false),
                    DisplayMode::Waterfall => (false, true),
                };
                let inner_gap = if show_spec && show_water { gap } else { 0.0 };
                let usable    = (band_rect.height() - axis_h - inner_gap).max(40.0);
                let spec_frac = match mode {
                    DisplayMode::Panafall  => 0.35,
                    DisplayMode::Split     => self.split_fractions[r].clamp(0.15, 0.85),
                    DisplayMode::Spectrum  => 1.0,
                    DisplayMode::Waterfall => 0.0,
                };
                let spec_h  = if show_spec  { (usable * spec_frac).max(0.0) } else { 0.0 };
                let water_h = if show_water { usable - spec_h }              else { 0.0 };
                let spec_rect = Rect::from_min_size(
                    band_rect.min,
                    Vec2::new(band_rect.width(), spec_h),
                );
                let axis_rect = Rect::from_min_size(
                    Pos2::new(band_rect.min.x, band_rect.min.y + spec_h),
                    Vec2::new(band_rect.width(), axis_h),
                );
                let water_rect = Rect::from_min_size(
                    Pos2::new(band_rect.min.x, band_rect.min.y + spec_h + axis_h + inner_gap),
                    Vec2::new(band_rect.width(), water_h),
                );

                // Single response for this band — handles tune, pan, context menu.
                // Created early so we can read drag_delta before drawing.
                let spec_resp = ui.interact(
                    spec_rect,
                    egui::Id::new(("spec-tune", r)),
                    Sense::click_and_drag(),
                );

                // --- Zoom via modifier+scroll ---
                let is_over_spec = ui.ctx().pointer_hover_pos()
                    .is_some_and(|p| band_rect.contains(p));

                let zoom_scroll: f32 = ui.input(|i| {
                    if !is_over_spec { return 0.0; }
                    i.events.iter().filter_map(|e| {
                        if let egui::Event::MouseWheel { delta, modifiers, .. } = e {
                            if modifiers.alt || modifiers.ctrl || modifiers.command {
                                return Some(delta.y);
                            }
                        }
                        None
                    }).sum()
                });
                if zoom_scroll.abs() > 0.001 {
                    let step = if zoom_scroll > 0.0 { -0.1_f32 } else { 0.1 };
                    self.view_states[r].z_factor =
                        (self.view_states[r].z_factor + step).clamp(0.05, 1.0);
                }

                // --- Pan via Middle-drag ---
                if spec_resp.dragged_by(egui::PointerButton::Middle) {
                    let delta_x = spec_resp.drag_delta().x;
                    let vis_w = (hi_bin - lo_bin) as f32;
                    let max_lo = (n_bins as f32 - vis_w).max(1.0);
                    let p_delta = -delta_x / spec_rect.width() * vis_w / max_lo;
                    self.view_states[r].p_slider =
                        (self.view_states[r].p_slider + p_delta).clamp(0.0, 1.0);
                }

                // --- Passband drag hit-test (checked before tune) ---
                let rx_state = self.app.rx(r).cloned().unwrap_or_default();
                let center_f  = center_hz as f32;
                let filter_lo_abs = center_f + rx_state.filter_lo as f32;
                let filter_hi_abs = center_f + rx_state.filter_hi as f32;
                let visible_span  = hi_hz - lo_hz;

                // Pixel x of each filter edge (clamped to spec_rect)
                let (x_flo, x_fhi) = if visible_span > 0.0 && filter_hi_abs > filter_lo_abs {
                    let to_x = |hz: f32| {
                        spec_rect.left()
                            + ((hz - lo_hz) / visible_span).clamp(0.0, 1.0) * spec_rect.width()
                    };
                    (to_x(filter_lo_abs), to_x(filter_hi_abs))
                } else {
                    (spec_rect.left(), spec_rect.right())
                };

                const EDGE_TOL: f32 = 5.0;
                let hp = ui.ctx().pointer_hover_pos();
                let hover_lo = hp.is_some_and(|p| {
                    (p.x - x_flo).abs() < EDGE_TOL && spec_rect.contains(p)
                });
                let hover_hi = hp.is_some_and(|p| {
                    (p.x - x_fhi).abs() < EDGE_TOL && spec_rect.contains(p)
                });
                let hover_center = hp.is_some_and(|p| {
                    p.x > x_flo + EDGE_TOL
                        && p.x < x_fhi - EDGE_TOL
                        && spec_rect.contains(p)
                });

                // Cursor icon
                if hover_lo
                    || hover_hi
                    || matches!(
                        self.passband_drags[r],
                        PassbandDrag::LoEdge | PassbandDrag::HiEdge
                    )
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                } else if hover_center
                    || matches!(self.passband_drags[r], PassbandDrag::Center)
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }

                // Detect drag start: classify which part was clicked
                let press_origin = ui.input(|i| i.pointer.press_origin());
                if spec_resp.drag_started() {
                    self.passband_drags[r] = PassbandDrag::None;
                    if let Some(o) = press_origin {
                        if spec_rect.contains(o) {
                            if (o.x - x_flo).abs() < EDGE_TOL {
                                self.passband_drags[r] = PassbandDrag::LoEdge;
                            } else if (o.x - x_fhi).abs() < EDGE_TOL {
                                self.passband_drags[r] = PassbandDrag::HiEdge;
                            } else if o.x > x_flo && o.x < x_fhi {
                                self.passband_drags[r] = PassbandDrag::Center;
                            }
                        }
                    }
                }
                if !spec_resp.dragged() {
                    self.passband_drags[r] = PassbandDrag::None;
                }

                // Apply drag: convert pixel delta → Hz delta → new offsets
                let passband_active =
                    !matches!(self.passband_drags[r], PassbandDrag::None);
                if passband_active && spec_resp.dragged() {
                    let delta_hz =
                        spec_resp.drag_delta().x / spec_rect.width() * visible_span;
                    let lo_off = rx_state.filter_lo as f32;
                    let hi_off = rx_state.filter_hi as f32;
                    let (mut new_lo, mut new_hi) = match self.passband_drags[r] {
                        PassbandDrag::LoEdge  => (lo_off + delta_hz, hi_off),
                        PassbandDrag::HiEdge  => (lo_off, hi_off + delta_hz),
                        PassbandDrag::Center  => (lo_off + delta_hz, hi_off + delta_hz),
                        PassbandDrag::None    => (lo_off, hi_off),
                    };
                    // Enforce minimum 100 Hz bandwidth
                    if new_hi - new_lo < 100.0 {
                        match self.passband_drags[r] {
                            PassbandDrag::LoEdge => new_lo = new_hi - 100.0,
                            PassbandDrag::HiEdge => new_hi = new_lo + 100.0,
                            _ => {}
                        }
                    }
                    self.app.set_rx_filter(r as u8, new_lo as f64, new_hi as f64);
                }

                // --- Spectrum draw (zoomed slice) ---
                let vis_bins = &snapshot.rx[r].spectrum_bins_db
                    [lo_bin.min(n_bins)..hi_bin.min(n_bins)];
                let ds = self.app.display_settings();
                if show_spec {
                    draw_spectrum_ex(
                        ui, spec_rect, vis_bins,
                        ds.spectrum_min_db, ds.spectrum_max_db,
                        false, // fill disabled — convex_polygon glitches on non-convex shapes
                    );
                    // Bandplan painted AFTER the spectrum background/grid so the
                    // semi-transparent tints are visible, but before the trace
                    // overlays so the curve still reads clearly on top.
                    bandplan::draw(
                        &ui.painter_at(spec_rect), spec_rect,
                        lo_hz, hi_hz, ds.bandplan_region,
                    );
                }

                // Peak hold trace (white, 1px)
                if show_spec && self.overlays[r].show_peak {
                    let peak_slice = &self.overlays[r].peak_bins
                        [lo_bin.min(n_bins)..hi_bin.min(n_bins)];
                    draw_trace_range(ui, spec_rect, peak_slice,
                        Color32::from_rgba_premultiplied(255, 255, 200, 180),
                        ds.spectrum_min_db, ds.spectrum_max_db);
                }
                // Average trace (cyan, 1px)
                if show_spec && self.overlays[r].show_avg {
                    let avg_slice = &self.overlays[r].avg_bins
                        [lo_bin.min(n_bins)..hi_bin.min(n_bins)];
                    draw_trace_range(ui, spec_rect, avg_slice,
                        Color32::from_rgba_premultiplied(100, 200, 255, 160),
                        ds.spectrum_min_db, ds.spectrum_max_db);
                }

                // AGC threshold line — only when AGC is on and we have a spectrum
                if show_spec {
                    let agc = self.app.rx(r).map(|v| v.agc_mode).unwrap_or(AgcPreset::Med);
                    draw_agc_line(
                        &ui.painter_at(spec_rect), spec_rect,
                        agc, ds.spectrum_min_db, ds.spectrum_max_db,
                    );
                }

                // RIT offset marker — yellow vertical line at center + rit_hz.
                if show_spec {
                    let rit = self.app.rx(r).map(|v| v.rit_hz).unwrap_or(0);
                    draw_rit_line(
                        &ui.painter_at(spec_rect), spec_rect,
                        rit, center_hz as f32, lo_hz, hi_hz,
                    );
                }

                // Notch / TNF markers — placeholder ±1 kHz demo notches when TNF is on.
                // Real positions come from WDSP once the TNF binding is exposed.
                if show_spec && self.app.rx(r).is_some_and(|v| v.tnf) {
                    let c = center_hz as f32;
                    let markers = [
                        NotchMarker { freq_hz: c - 1000.0, enabled: true },
                        NotchMarker { freq_hz: c + 1000.0, enabled: true },
                    ];
                    draw_notch_markers(
                        &ui.painter_at(spec_rect), spec_rect,
                        &markers, lo_hz, hi_hz,
                    );
                }

                // Passband overlay with hover/drag feedback
                if show_spec && visible_span > 0.0 && x_fhi > x_flo + 1.0 {
                    let band_rect = Rect::from_min_max(
                        egui::pos2(x_flo, spec_rect.top()),
                        egui::pos2(x_fhi, spec_rect.bottom()),
                    );
                    let painter = ui.painter_at(spec_rect);
                    painter.rect_filled(
                        band_rect, 0.0,
                        Color32::from_rgba_premultiplied(80, 200, 255, 30),
                    );
                    let lo_col = if hover_lo
                        || matches!(self.passband_drags[r], PassbandDrag::LoEdge)
                    {
                        Color32::WHITE
                    } else {
                        Color32::from_rgba_premultiplied(0, 0, 200, 200)
                    };
                    let hi_col = if hover_hi
                        || matches!(self.passband_drags[r], PassbandDrag::HiEdge)
                    {
                        Color32::WHITE
                    } else {
                        Color32::from_rgba_premultiplied(0, 0, 200, 200)
                    };
                    painter.line_segment(
                        [egui::pos2(x_flo, spec_rect.top()), egui::pos2(x_flo, spec_rect.bottom())],
                        Stroke::new(1.5, lo_col),
                    );
                    painter.line_segment(
                        [egui::pos2(x_fhi, spec_rect.top()), egui::pos2(x_fhi, spec_rect.bottom())],
                        Stroke::new(1.5, hi_col),
                    );
                }

                // Frequency axis strip
                draw_freq_axis(&ui.painter_at(axis_rect), axis_rect, lo_hz, hi_hz);

                // RX label
                let is_active = r == self.app.active_rx();
                let prefix = if is_active && num_rx > 1 { "▶ " } else { "" };
                let zoom_label = if self.view_states[r].z_factor < 0.99 {
                    format!("  [{:.0}×]", 1.0 / self.view_states[r].z_factor)
                } else {
                    String::new()
                };
                // RX label: on spectrum if shown, otherwise on waterfall top-left.
                let label_rect = if show_spec { spec_rect } else { water_rect };
                ui.painter_at(label_rect).text(
                    label_rect.min + Vec2::new(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    format!("{}RX{}  {:.3} MHz  {:?}{}",
                        prefix,
                        r + 1,
                        center_hz as f64 / 1.0e6,
                        snapshot.rx[r].mode,
                        zoom_label),
                    egui::FontId::monospace(12.0),
                    if snapshot.rx[r].enabled { Color32::LIGHT_GREEN } else { Color32::GRAY },
                );

                // Waterfall (UV-cropped for zoom)
                if show_water {
                    self.waterfalls[r].draw(ui, ctx, water_rect, uv_lo, uv_hi);
                }

                // VFO marker at correct position within the visible window
                let vfo_frac = if (hi_hz - lo_hz).abs() > 0.1 {
                    (center_hz as f32 - lo_hz) / (hi_hz - lo_hz)
                } else {
                    0.5
                };
                if show_spec {
                    let vfo_spec_x = spec_rect.left() + vfo_frac * spec_rect.width();
                    draw_vfo_marker(&ui.painter_at(spec_rect), spec_rect, vfo_spec_x);
                }
                if show_water {
                    let vfo_water_x = water_rect.left() + vfo_frac * water_rect.width();
                    draw_vfo_marker(&ui.painter_at(water_rect), water_rect, vfo_water_x);
                }

                // --- Tune interaction + context menu (skipped during passband drag) ---
                spec_resp.context_menu(|ui| {
                    ui.checkbox(&mut self.overlays[r].show_peak, "Peak hold");
                    ui.checkbox(&mut self.overlays[r].show_avg,  "Average");
                    if ui.button("Reset zoom").clicked() {
                        self.view_states[r] = RxViewState::default();
                        ui.close();
                    }
                });
                if show_spec && !passband_active {
                    let (new_freq, clicked) = tune_from_response(
                        &spec_resp, ui, spec_rect, center_hz, lo_hz, hi_hz,
                    );
                    if let Some(f) = new_freq { pending_tunes.push((r, f)); }
                    if clicked { newly_active = Some(r); }
                }

                if show_water {
                    let (new_freq, clicked) = handle_tune_input(
                        ui, water_rect,
                        egui::Id::new(("water-tune", r)),
                        center_hz, lo_hz, hi_hz,
                    );
                    if let Some(f) = new_freq { pending_tunes.push((r, f)); }
                    if clicked { newly_active = Some(r); }
                }

                // --- Split-mode draggable divider ---
                if matches!(mode, DisplayMode::Split) {
                    let divider = Rect::from_min_size(
                        Pos2::new(band_rect.min.x, band_rect.min.y + spec_h - 2.0),
                        Vec2::new(band_rect.width(), axis_h + 4.0),
                    );
                    let div_resp = ui.interact(
                        divider,
                        egui::Id::new(("split-divider", r)),
                        Sense::click_and_drag(),
                    );
                    if div_resp.hovered() || div_resp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    }
                    if div_resp.dragged() {
                        let dy = div_resp.drag_delta().y;
                        let frac = self.split_fractions[r]
                            + dy / (band_rect.height() - axis_h).max(1.0);
                        self.split_fractions[r] = frac.clamp(0.15, 0.85);
                    }
                }

                // --- 4-icon mode toggle strip (top-right corner) ---
                {
                    let modes_btn: [(DisplayMode, &str, &str); 4] = [
                        (DisplayMode::Panafall,  "P", "Panafall (spectrum + waterfall)"),
                        (DisplayMode::Spectrum,  "S", "Spectrum only"),
                        (DisplayMode::Waterfall, "W", "Waterfall only"),
                        (DisplayMode::Split,     "≡", "Split with draggable divider"),
                    ];
                    let btn_w = 18.0_f32;
                    let btn_h = 16.0_f32;
                    let pad   = 2.0_f32;
                    let total_w = btn_w * modes_btn.len() as f32 + pad * (modes_btn.len() - 1) as f32;
                    let strip_x = band_rect.right() - total_w - 4.0;
                    let strip_y = band_rect.top() + 2.0;
                    for (i, (m, glyph, tip)) in modes_btn.iter().enumerate() {
                        let x = strip_x + (btn_w + pad) * i as f32;
                        let btn = Rect::from_min_size(
                            Pos2::new(x, strip_y),
                            Vec2::new(btn_w, btn_h),
                        );
                        let resp = ui.interact(
                            btn,
                            egui::Id::new(("disp-mode-btn", r, i)),
                            Sense::click(),
                        ).on_hover_text(*tip);
                        let selected = *m == mode;
                        let bg = if selected {
                            Color32::from_rgb(60, 110, 60)
                        } else if resp.hovered() {
                            Color32::from_rgba_unmultiplied(70, 70, 70, 220)
                        } else {
                            Color32::from_rgba_unmultiplied(30, 30, 30, 200)
                        };
                        let painter = ui.painter_at(btn);
                        painter.rect_filled(btn, 2.0, bg);
                        painter.rect_stroke(
                            btn, 2.0,
                            Stroke::new(1.0, Color32::from_gray(90)),
                            egui::StrokeKind::Inside,
                        );
                        painter.text(
                            btn.center(),
                            egui::Align2::CENTER_CENTER,
                            *glyph,
                            egui::FontId::proportional(11.0),
                            Color32::WHITE,
                        );
                        if resp.clicked() {
                            self.display_modes[r] = *m;
                        }
                    }
                }
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
    // Force dark theme regardless of the OS preference (Windows follows
    // the system theme by default and would otherwise override visuals).
    ctx.set_theme(egui::ThemePreference::Dark);

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

fn draw_vfo_marker(painter: &egui::Painter, rect: Rect, x: f32) {
    let x = x.clamp(rect.left(), rect.right());
    painter.line_segment(
        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
        Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 160)),
    );
}

/// Handle click-to-tune and scroll-to-tune on a rect.
/// `lo_hz` / `hi_hz` define the currently-visible Hz range (absolute).
/// Ctrl+scroll is reserved for zoom and is skipped here.
fn handle_tune_input(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    center_hz: u32,
    lo_hz: f32,
    hi_hz: f32,
) -> (Option<u32>, bool) {
    let response = ui.interact(rect, id, Sense::click_and_drag());

    let mut new_freq: Option<u32> = None;
    let clicked = response.clicked() || response.dragged();

    if clicked {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let click_hz = lo_hz + t * (hi_hz - lo_hz);
            let next = (click_hz.round() as i64).max(0) as u32;
            if next != center_hz {
                new_freq = Some(next);
            }
        }
    }

    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        let modifiers = ui.input(|i| i.modifiers);
        // Ctrl+scroll = zoom (handled in draw_main). Skip tune here.
        if scroll_y.abs() > 0.5 && !modifiers.ctrl {
            let step_hz: i64 = if modifiers.shift { 100 } else { 10 };
            let ticks = (scroll_y / 50.0).round() as i64;
            let ticks = if ticks == 0 { if scroll_y > 0.0 { 1 } else { -1 } } else { ticks };
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
/// `lo_hz` / `hi_hz` define the currently-visible Hz range (absolute).
/// Ctrl+scroll is reserved for zoom and is skipped here.
fn tune_from_response(
    response: &egui::Response,
    ui: &egui::Ui,
    rect: Rect,
    center_hz: u32,
    lo_hz: f32,
    hi_hz: f32,
) -> (Option<u32>, bool) {
    let mut new_freq: Option<u32> = None;
    let clicked = response.clicked() || response.dragged();

    if clicked {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let click_hz = lo_hz + t * (hi_hz - lo_hz);
            let next = (click_hz.round() as i64).max(0) as u32;
            if next != center_hz {
                new_freq = Some(next);
            }
        }
    }

    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        let modifiers = ui.input(|i| i.modifiers);
        // Ctrl+scroll = zoom (handled in draw_main). Skip tune here.
        if scroll_y.abs() > 0.5 && !modifiers.ctrl {
            let step_hz: i64 = if modifiers.shift { 100 } else { 10 };
            let ticks = (scroll_y / 50.0).round() as i64;
            let ticks = if ticks == 0 { if scroll_y > 0.0 { 1 } else { -1 } } else { ticks };
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

// --- Spectrum (immediate-mode line draw) --------------------------------

/// Vertical yellow line at the RIT offset (center + rit_hz), with a label
/// like "RIT +250 Hz". Skipped when rit_hz is 0 or the line falls outside
/// the visible window. Arion extension — Thetis shows RIT only as a number.
fn draw_rit_line(
    painter: &egui::Painter,
    rect: Rect,
    rit_hz: i32,
    center_hz: f32,
    lo_hz: f32,
    hi_hz: f32,
) {
    if rit_hz == 0 { return; }
    let span = hi_hz - lo_hz;
    if span <= 0.0 { return; }
    let x = rect.left() + (center_hz + rit_hz as f32 - lo_hz) / span * rect.width();
    if x < rect.left() || x > rect.right() { return; }
    let color = Color32::YELLOW;
    painter.vline(x, rect.y_range(), Stroke::new(1.0, color));
    let label = format!("RIT {rit_hz:+} Hz");
    let align = if x > rect.right() - 70.0 {
        egui::Align2::RIGHT_TOP
    } else {
        egui::Align2::LEFT_TOP
    };
    let anchor_x = if matches!(align, egui::Align2::RIGHT_TOP) { x - 2.0 } else { x + 2.0 };
    painter.text(
        Pos2::new(anchor_x, rect.top() + 2.0),
        align,
        label,
        egui::FontId::proportional(9.0),
        color,
    );
}

/// Display-only notch marker. Until the WDSP TNF binding is exposed, the
/// egui frontend constructs placeholder markers from `rx.tnf` + center_hz.
#[derive(Clone, Copy)]
struct NotchMarker {
    /// Absolute Hz (not offset from center).
    freq_hz: f32,
    enabled: bool,
}

/// Vertical lines + "N" label at each notch. Olive when active, gray when
/// disabled — matches Thetis. Skips notches outside the visible window.
fn draw_notch_markers(
    painter: &egui::Painter,
    rect: Rect,
    markers: &[NotchMarker],
    lo_hz: f32,
    hi_hz: f32,
) {
    let span = hi_hz - lo_hz;
    if span <= 0.0 { return; }
    for m in markers {
        let x = rect.left() + (m.freq_hz - lo_hz) / span * rect.width();
        if x < rect.left() || x > rect.right() { continue; }
        let color = if m.enabled {
            Color32::from_rgb(128, 128, 0) // Olive
        } else {
            Color32::from_gray(128)
        };
        painter.vline(x, rect.y_range(), Stroke::new(1.0, color));
        painter.text(
            Pos2::new(x + 2.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            "N",
            egui::FontId::proportional(9.0),
            color,
        );
    }
}

/// Horizontal orange line at the AGC threshold dBFS. Skipped when AGC is Off.
/// Thresholds are WDSP defaults (Long/Slow -100, Med -90, Fast -80).
fn draw_agc_line(
    painter: &egui::Painter,
    rect: Rect,
    agc: AgcPreset,
    min_db: f32,
    max_db: f32,
) {
    let thresh_db: f32 = match agc {
        AgcPreset::Off              => return,
        AgcPreset::Long             => -100.0,
        AgcPreset::Slow             => -100.0,
        AgcPreset::Med              =>  -90.0,
        AgcPreset::Fast             =>  -80.0,
    };
    if (min_db - max_db).abs() < 1.0 { return; }
    let t = (thresh_db - max_db) / (min_db - max_db);
    let y = rect.top() + t * rect.height();
    if y < rect.top() + 1.0 || y > rect.bottom() - 1.0 { return; }
    let color = Color32::from_rgb(255, 140, 0);
    painter.hline(rect.x_range(), y, Stroke::new(1.0, color));
    painter.text(
        Pos2::new(rect.right() - 4.0, y - 1.0),
        egui::Align2::RIGHT_BOTTOM,
        "AGC",
        egui::FontId::proportional(10.0),
        color,
    );
}

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

    // Fill under the curve (Arion PanFill)
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

// --- Zoom / pan view state -----------------------------------------------

/// Which part of the filter passband is currently being dragged.
#[derive(Default, Clone, Copy, PartialEq)]
enum PassbandDrag {
    #[default]
    None,
    LoEdge,
    HiEdge,
    Center,
}

/// Per-RX display layout. Panafall is the classic spectrum+waterfall view
/// with a fixed 35/65 split; Split lets the user drag the divider.
#[derive(Default, Clone, Copy, PartialEq)]
enum DisplayMode {
    #[default]
    Panafall,
    Spectrum,
    Waterfall,
    Split,
}

/// Per-RX display zoom + pan state. Pure display — never moves the VFO.
#[derive(Clone)]
struct RxViewState {
    /// Zoom factor ∈ [0.05, 1.0].  1.0 = full span; 0.05 = 5% of span.
    /// Perceptually linear: `t = log10(9·z + 1)` matches Thetis exactly.
    z_factor: f32,
    /// Pan position ∈ [0, 1].  0 = anchored at left edge; 1 = right edge.
    p_slider: f32,
}

impl Default for RxViewState {
    fn default() -> Self {
        RxViewState { z_factor: 1.0, p_slider: 0.5 }
    }
}

impl RxViewState {
    /// Return the `[lo_bin, hi_bin)` slice of the spectrum that is
    /// currently visible given the zoom + pan.
    ///
    /// `z_factor = 1.0` → all bins visible (no zoom).
    /// `z_factor = 0.05` → 5 % of bins visible (20× zoom).
    fn visible_bins(&self, n_bins: usize) -> (usize, usize) {
        let vis = ((n_bins as f32 * self.z_factor) as usize)
            .max(2)
            .min(n_bins);
        let max_lo = n_bins - vis;
        let lo = ((self.p_slider * max_lo as f32) as usize).min(max_lo);
        (lo, lo + vis)
    }
}

// --- Frequency axis -------------------------------------------------------

/// Draw a frequency-labelled axis strip.
/// The strip is `rect` (typically 18 px tall), `lo_hz`…`hi_hz` are the
/// absolute Hz boundaries of the currently-visible spectrum window.
fn draw_freq_axis(painter: &egui::Painter, rect: Rect, lo_hz: f32, hi_hz: f32) {
    let span = hi_hz - lo_hz;
    if span <= 0.0 || rect.width() < 4.0 {
        return;
    }
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(8, 10, 14));

    // Adaptive tick step: pick the smallest step giving ≤10 ticks.
    // Candidates: 1, 2, 2.5, 5 × successive powers of 10.
    const STEPS: [f32; 4] = [1.0, 2.0, 2.5, 5.0];
    let mut tick_hz = 1.0_f32;
    'find: for exp in -1_i32..=10 {
        let scale = 10.0_f32.powi(exp);
        for &s in &STEPS {
            let candidate = s * scale;
            if span / candidate <= 10.0 {
                tick_hz = candidate;
                break 'find;
            }
        }
    }

    // Decimal places to display: enough to resolve tick_hz in MHz.
    let decimals: usize = if tick_hz >= 1_000_000.0 { 0 }
        else if tick_hz >= 100_000.0 { 1 }
        else if tick_hz >= 10_000.0  { 2 }
        else                          { 3 };

    // Iterate by integer tick index to avoid floating-point drift and
    // use integer modulo for major/minor classification (no flicker).
    let first_n = (lo_hz / tick_hz).ceil() as i64;
    let last_n  = (hi_hz / tick_hz).floor() as i64;
    for n in first_n..=last_n {
        let f = n as f64 * tick_hz as f64;  // f64 for sub-Hz precision
        let x = rect.left() + ((f as f32 - lo_hz) / span) * rect.width();
        // Major tick every 5 minor ticks — stable integer test
        let is_major = n % 5 == 0;
        let tick_h: f32 = if is_major { 6.0 } else { 4.0 };
        painter.line_segment(
            [egui::Pos2::new(x, rect.top()), egui::Pos2::new(x, rect.top() + tick_h)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
        );
        if is_major {
            let label = format!("{:.prec$}", f / 1_000_000.0, prec = decimals);
            painter.text(
                egui::Pos2::new(x, rect.top() + tick_h + 1.0),
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::monospace(8.0),
                egui::Color32::from_gray(150),
            );
        }
    }
}

// --- Waterfall (egui-specific texture cache) ----------------------------

/// Per-RX scrolling waterfall display, owned by `EguiView` rather than
/// `App` because the `TextureHandle` is a frontend-specific resource.
struct Waterfall {
    pixels:  Vec<Color32>,
    texture: Option<TextureHandle>,
    /// Unique index used to give each waterfall its own texture name
    /// so egui doesn't confuse them when multiple RX bands are active.
    rx_idx:  usize,
    /// Frame counter: incremented every call; a row is only pushed when
    /// `frame_counter % speed == 0` (speed ∈ [1, 8]).
    frame_counter: u32,
    /// Total rows actually pushed (mod 2^32 — only the low bits matter).
    rows_pushed: u32,
    /// UTC second (Unix epoch) recorded each time a row is actually pushed.
    /// Front = oldest (top of waterfall), back = newest (bottom).
    row_timestamps: std::collections::VecDeque<u64>,
}

impl Waterfall {
    const WIDTH:  usize = SPECTRUM_BINS;
    const HEIGHT: usize = 256;

    fn new(rx_idx: usize) -> Self {
        Waterfall {
            pixels:  vec![Color32::BLACK; Self::WIDTH * Self::HEIGHT],
            texture: None,
            rx_idx,
            frame_counter: 0,
            rows_pushed: 0,
            row_timestamps: std::collections::VecDeque::with_capacity(Self::HEIGHT),
        }
    }

    /// Push a new spectrum row at the given `speed` (1 = every frame, 8 = every 8th frame).
    fn push_row(&mut self, bins_db: &[f32], min_db: f32, max_db: f32,
                palette: WaterfallPalette, speed: u8) {
        let speed = speed.max(1) as u32;
        self.frame_counter = self.frame_counter.wrapping_add(1);
        if !self.frame_counter.is_multiple_of(speed) {
            return;
        }

        let row_len = Self::WIDTH;
        self.pixels.copy_within(row_len.., 0);

        let new_row_start = (Self::HEIGHT - 1) * row_len;
        let new_row = &mut self.pixels[new_row_start..new_row_start + row_len];
        let n = bins_db.len().min(row_len);
        for (i, px) in new_row.iter_mut().enumerate().take(n) {
            let src_idx = (i * bins_db.len()) / row_len;
            *px = db_to_waterfall_color(bins_db[src_idx], min_db, max_db, palette);
        }

        // Record timestamp: shift old entries up (pop front if full), push newest at back.
        if self.row_timestamps.len() >= Self::HEIGHT {
            self.row_timestamps.pop_front();
        }
        let unix_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.row_timestamps.push_back(unix_now);
        self.rows_pushed = self.rows_pushed.wrapping_add(1);
    }

    /// Draw the waterfall into `rect`.
    /// `uv_lo` / `uv_hi` ∈ [0, 1] control horizontal UV cropping for
    /// zoom: 0.0 = left edge of the full spectrum, 1.0 = right edge.
    fn draw(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, rect: Rect, uv_lo: f32, uv_hi: f32) {
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);

        let image = ColorImage {
            size:   [Self::WIDTH, Self::HEIGHT],
            pixels: self.pixels.clone(),
            source_size: Vec2::new(Self::WIDTH as f32, Self::HEIGHT as f32),
        };
        let label = format!("waterfall_{}", self.rx_idx);
        let tex = self.texture.get_or_insert_with(|| {
            ctx.load_texture(&label, image.clone(), TextureOptions::LINEAR)
        });
        tex.set(image, TextureOptions::LINEAR);

        let uv_lo = uv_lo.clamp(0.0, 1.0);
        let uv_hi = uv_hi.clamp(uv_lo, 1.0);
        let painter = ui.painter_at(rect);
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::new(uv_lo, 0.0), Pos2::new(uv_hi, 1.0)),
            Color32::WHITE,
        );

        // Time labels on the right edge.
        // Labels scroll with the waterfall: phase = rows_pushed % step_rows shifts
        // all label positions by one row each time a new row is pushed, so labels
        // appear to drift upward in sync with the data. Alpha: unmultiplied so that
        // low-alpha values actually become transparent (premultiplied with r>a is invalid).
        let n = self.row_timestamps.len();
        if n >= 2 {
            let px_per_row = rect.height() / Self::HEIGHT as f32;
            let step_rows = ((60.0 / px_per_row).round() as usize).max(1);
            let phase = self.rows_pushed as usize % step_rows;

            for k in 0.. {
                let rows_from_bottom = k * step_rows + phase;
                if rows_from_bottom >= Self::HEIGHT { break; }

                // Alpha: bright at bottom (rows_from_bottom small), fades toward top.
                let alpha = ((1.0 - rows_from_bottom as f32 / Self::HEIGHT as f32)
                    * 210.0) as u8;
                if alpha < 20 { break; }

                // Newest timestamp = n-1 (bottom). Go back by rows_from_bottom.
                let ts_idx = match n.checked_sub(1 + rows_from_bottom) {
                    Some(i) => i,
                    None => break,
                };
                let unix_ts = self.row_timestamps[ts_idx];
                let hh = (unix_ts / 3600) % 24;
                let mm = (unix_ts / 60) % 60;
                let ss = unix_ts % 60;

                let screen_y = rect.bottom() - rows_from_bottom as f32 * px_per_row;
                painter.text(
                    egui::pos2(rect.right() - 4.0, screen_y),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("{hh:02}:{mm:02}:{ss:02}Z"),
                    egui::FontId::monospace(9.0),
                    Color32::from_rgba_unmultiplied(200, 200, 200, alpha),
                );
            }
        }
    }
}

/// Linearly interpolate between two RGB colours at parameter `t` ∈ [0,1].
#[inline]
fn lerp_color(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> Color32 {
    let r = (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t).round() as u8;
    let g = (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t).round() as u8;
    let b_ = (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t).round() as u8;
    Color32::from_rgb(r, g, b_)
}

/// Interpolate across a list of `(stop_t, r, g, b)` stops.
#[inline]
fn lerp_stops(stops: &[(f32, u8, u8, u8)], t: f32) -> Color32 {
    if stops.len() < 2 { return Color32::BLACK; }
    // Find bracketing pair
    let mut lo = &stops[0];
    let mut hi = &stops[1];
    for i in 1..stops.len() {
        if t <= stops[i].0 {
            lo = &stops[i - 1];
            hi = &stops[i];
            break;
        }
        lo = &stops[i];
        hi = &stops[i];
    }
    let span = hi.0 - lo.0;
    let local_t = if span > 0.0 { ((t - lo.0) / span).clamp(0.0, 1.0) } else { 0.0 };
    lerp_color((lo.1, lo.2, lo.3), (hi.1, hi.2, hi.3), local_t)
}

fn db_to_waterfall_color(db: f32, min_db: f32, max_db: f32, palette: WaterfallPalette) -> Color32 {
    let range = max_db - min_db;
    let t = if range.abs() < 0.001 { 0.0 } else { ((db - min_db) / range).clamp(0.0, 1.0) };

    match palette {
        WaterfallPalette::Enhanced => {
            // Exact Thetis "Enhanced" palette (PanDisplay.cs:2470-2635)
            const STOPS: &[(f32, u8, u8, u8)] = &[
                (0.0 / 9.0, 0,   0,   0),
                (2.0 / 9.0, 0,   0,   255),
                (3.0 / 9.0, 0,   255, 255),
                (4.0 / 9.0, 0,   255, 0),
                (5.0 / 9.0, 255, 255, 0),
                (7.0 / 9.0, 255, 0,   0),
                (8.0 / 9.0, 255, 0,   255),
                (1.0,        192, 124, 255),
            ];
            lerp_stops(STOPS, t)
        }
        WaterfallPalette::Classic => {
            // Blue → magenta → red → orange (previous Arion style)
            const STOPS: &[(f32, u8, u8, u8)] = &[
                (0.00, 0,   0,   128),
                (0.50, 180, 0,   180),
                (0.75, 200, 0,   0),
                (1.00, 255, 140, 0),
            ];
            lerp_stops(STOPS, t)
        }
        WaterfallPalette::Greyscale => {
            let v = (t * 255.0).round() as u8;
            Color32::from_gray(v)
        }
        WaterfallPalette::Thermal => {
            const STOPS: &[(f32, u8, u8, u8)] = &[
                (0.00, 0,   0,   0),
                (0.30, 128, 0,   128),
                (0.60, 255, 64,  0),
                (0.85, 255, 200, 0),
                (1.00, 255, 255, 255),
            ];
            lerp_stops(STOPS, t)
        }
        WaterfallPalette::Spectran => {
            // Black → dark green → bright green (Spectran-style)
            const STOPS: &[(f32, u8, u8, u8)] = &[
                (0.00, 0,  0,   0),
                (0.40, 0,  80,  0),
                (0.70, 0,  200, 50),
                (1.00, 180, 255, 100),
            ];
            lerp_stops(STOPS, t)
        }
    }
}
