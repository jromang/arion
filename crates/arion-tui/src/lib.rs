//! ratatui console frontend for Arion — rich layout matching the
//! egui desktop version. Waterfall, spectrum, side panel, DSP controls,
//! REPL, popups, mouse support, keyboard navigation.

mod waterfall;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::*;

use arion_app::{App, AppOptions, Band, FilterPreset, AgcPreset, dbm_to_s_units, SMETER_DBFS_TO_DBM_OFFSET};
use arion_core::{WdspMode, MAX_RX, SPECTRUM_BINS};
use arion_script::ScriptEngine;
use waterfall::TuiWaterfall;

// --- Focus system -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Spectrum,
    SideMode,
    SideBand,
    SideFilter,
    SideDigital,
    Controls,
    Repl,
    Popup(PopupKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    Help,
    Memories,
}

const DIGITAL_OPTIONS: &[(Option<arion_core::DigitalMode>, &str)] = &[
    (None, "Off"),
    (Some(arion_core::DigitalMode::Psk31), "PSK31"),
    (Some(arion_core::DigitalMode::Psk63), "PSK63"),
    (Some(arion_core::DigitalMode::Rtty),  "RTTY"),
    (Some(arion_core::DigitalMode::Aprs),  "APRS"),
    (Some(arion_core::DigitalMode::Ft8),   "FT8"),
];

const FOCUS_ORDER: &[Focus] = &[
    Focus::Spectrum,
    Focus::SideMode,
    Focus::SideBand,
    Focus::SideFilter,
    Focus::SideDigital,
    Focus::Controls,
];

// --- TuiView ------------------------------------------------------------

pub struct TuiView {
    app: App,
    script: ScriptEngine,
    repl_input: String,
    repl_visible: bool,
    focus: Focus,
    // Side panel list state
    mode_state: ListState,
    band_state: ListState,
    filter_state: ListState,
    digital_state: ListState,
    // Memories table state
    mem_state: TableState,
    // Waterfall
    waterfalls: Vec<TuiWaterfall>,
    // Hit areas for mouse interaction (updated each draw)
    last_spectrum_area: Rect,
    last_mode_area: Rect,
    last_band_area: Rect,
    last_filter_area: Rect,
    last_digital_area: Rect,
    /// Rolling buffer of recent digital decodes across all RX — shown
    /// at the bottom of the side panel. Telemetry only carries
    /// decodes that fired during the last snapshot interval, so the
    /// TUI keeps its own bounded history.
    digital_history: Vec<String>,
    last_controls_area: Rect,
}

impl TuiView {
    pub fn new(opts: AppOptions) -> Self {
        let waterfalls = (0..MAX_RX)
            .map(|_| TuiWaterfall::new(SPECTRUM_BINS, 64))
            .collect();
        TuiView {
            app: App::new(opts),
            script: ScriptEngine::default(),
            repl_input: String::new(),
            repl_visible: false,
            focus: Focus::Spectrum,
            mode_state: ListState::default(),
            band_state: ListState::default(),
            filter_state: ListState::default(),
            digital_state: ListState::default(),
            mem_state: TableState::default(),
            waterfalls,
            last_spectrum_area: Rect::default(),
            last_mode_area: Rect::default(),
            last_band_area: Rect::default(),
            last_filter_area: Rect::default(),
            last_digital_area: Rect::default(),
            digital_history: Vec::with_capacity(32),
            last_controls_area: Rect::default(),
        }
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        stdout.execute(EnableMouseCapture)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

        let tick_rate = Duration::from_millis(50);

        loop {
            self.app.tick(Instant::now());

            // Push waterfall rows from telemetry
            if let Some(snapshot) = self.app.telemetry_snapshot() {
                for r in 0..snapshot.num_rx.min(MAX_RX as u8) as usize {
                    self.waterfalls[r].push_row(&snapshot.rx[r].spectrum_bins_db);
                }
            }

            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(tick_rate)? {
                match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                        if self.handle_key(key) {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => {
                        self.handle_mouse(mouse);
                    }
                    _ => {}
                }
            }
        }

        disable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(DisableMouseCapture)?;
        stdout.execute(LeaveAlternateScreen)?;
        self.app.shutdown();
        Ok(())
    }

    // --- Input handling -------------------------------------------------

    fn handle_key(&mut self, key: event::KeyEvent) -> bool {
        // Popup mode: consume all keys
        if let Focus::Popup(popup) = self.focus {
            return self.handle_popup_key(key, popup);
        }

        // REPL mode
        if self.focus == Focus::Repl {
            return self.handle_repl_key(key);
        }

        tracing::debug!(?key, focus=?self.focus, "key event");

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') => {
                if self.app.is_connected() { self.app.disconnect(); }
                else { self.app.connect(); }
            }
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Char(':') | KeyCode::F(9) => {
                self.repl_visible = true;
                self.focus = Focus::Repl;
            }
            KeyCode::F(1) => self.focus = Focus::Popup(PopupKind::Help),
            KeyCode::F(5) => {
                self.mem_state.select(Some(0));
                self.focus = Focus::Popup(PopupKind::Memories);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.tune_step(100),
            KeyCode::Char('-') => self.tune_step(-100),
            KeyCode::PageUp => self.tune_step(1000),
            KeyCode::PageDown => self.tune_step(-1000),
            KeyCode::Char('n') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.nr3).unwrap_or(false);
                self.app.set_rx_nr3(rx, on);
            }
            KeyCode::Char('N') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.nr4).unwrap_or(false);
                self.app.set_rx_nr4(rx, on);
            }
            KeyCode::Char('l') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.locked).unwrap_or(false);
                self.app.set_rx_locked(rx, on);
            }
            KeyCode::Char('r') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.anr).unwrap_or(false);
                self.app.set_rx_anr(rx, on);
            }
            KeyCode::Char('R') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.emnr).unwrap_or(false);
                self.app.set_rx_emnr(rx, on);
            }
            KeyCode::Char('s') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.squelch).unwrap_or(false);
                self.app.set_rx_squelch(rx, on);
            }
            KeyCode::Char('p') => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.apf).unwrap_or(false);
                self.app.set_rx_apf(rx, on);
            }
            KeyCode::Char('a') => self.cycle_agc(),
            KeyCode::Up => self.side_panel_up(),
            KeyCode::Down => self.side_panel_down(),
            KeyCode::Enter => self.side_panel_select(),
            _ => {}
        }
        false
    }

    fn handle_repl_key(&mut self, key: event::KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return true;
        }
        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::Spectrum;
            }
            KeyCode::Enter => {
                let line = self.repl_input.clone();
                self.repl_input.clear();
                self.script.run_line(&line, &mut self.app);
            }
            KeyCode::Char(c) => self.repl_input.push(c),
            KeyCode::Backspace => { self.repl_input.pop(); }
            _ => {}
        }
        false
    }

    fn handle_popup_key(&mut self, key: event::KeyEvent, popup: PopupKind) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::F(5) => {
                self.focus = Focus::Spectrum;
            }
            KeyCode::Enter if popup == PopupKind::Memories => {
                if let Some(idx) = self.mem_state.selected() {
                    self.app.load_memory(idx);
                }
                self.focus = Focus::Spectrum;
            }
            KeyCode::Up if popup == PopupKind::Memories => {
                let i = self.mem_state.selected().unwrap_or(0);
                self.mem_state.select(Some(i.saturating_sub(1)));
            }
            KeyCode::Down if popup == PopupKind::Memories => {
                let i = self.mem_state.selected().unwrap_or(0);
                let max = self.app.memories().len().saturating_sub(1);
                self.mem_state.select(Some((i + 1).min(max)));
            }
            KeyCode::Char('d') if popup == PopupKind::Memories => {
                if let Some(idx) = self.mem_state.selected() {
                    self.app.delete_memory(idx);
                }
            }
            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, mouse: event::MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;

        match mouse.kind {
            MouseEventKind::Down(event::MouseButton::Left) => {
                // Click-to-tune on spectrum
                if self.hit_test(self.last_spectrum_area, x, y) {
                    if let Some(snapshot) = self.app.telemetry_snapshot() {
                        let rx = self.app.active_rx();
                        if rx < snapshot.rx.len() {
                            let span = snapshot.rx[rx].span_hz as f32;
                            let center = snapshot.rx[rx].center_freq_hz;
                            let area = self.last_spectrum_area;
                            let dx_norm = (x as f32 - area.x as f32) / area.width as f32 - 0.5;
                            let new_freq = (center as f32 + dx_norm * span).max(0.0) as u32;
                            self.app.set_rx_frequency(rx as u8, new_freq);
                        }
                    }
                    self.focus = Focus::Spectrum;
                }
                // Click on Mode list
                else if self.hit_test(self.last_mode_area, x, y) {
                    let row = (y - self.last_mode_area.y).saturating_sub(1) as usize; // -1 for border
                    let modes = all_modes();
                    if row < modes.len() {
                        self.mode_state.select(Some(row));
                        self.app.set_rx_mode(self.app.active_rx() as u8, modes[row].0);
                    }
                    self.focus = Focus::SideMode;
                }
                // Click on Band list
                else if self.hit_test(self.last_band_area, x, y) {
                    let row = (y - self.last_band_area.y).saturating_sub(1) as usize;
                    if row < Band::ALL.len() {
                        self.band_state.select(Some(row));
                        self.app.jump_to_band(Band::ALL[row]);
                    }
                    self.focus = Focus::SideBand;
                }
                // Click on Filter list
                else if self.hit_test(self.last_filter_area, x, y) {
                    let row = (y - self.last_filter_area.y).saturating_sub(1) as usize;
                    if row < FilterPreset::ALL.len() {
                        self.filter_state.select(Some(row));
                        self.app.set_rx_filter_preset(self.app.active_rx() as u8, FilterPreset::ALL[row]);
                    }
                    self.focus = Focus::SideFilter;
                }
                // Click on Digital list
                else if self.hit_test(self.last_digital_area, x, y) {
                    let row = (y - self.last_digital_area.y).saturating_sub(1) as usize;
                    if row < DIGITAL_OPTIONS.len() {
                        self.digital_state.select(Some(row));
                        self.app.set_rx_digital_mode(
                            self.app.active_rx() as u8,
                            DIGITAL_OPTIONS[row].0,
                        );
                    }
                    self.focus = Focus::SideDigital;
                }
            }
            MouseEventKind::ScrollUp => self.tune_step(100),
            MouseEventKind::ScrollDown => self.tune_step(-100),
            _ => {}
        }
    }

    fn hit_test(&self, area: Rect, x: u16, y: u16) -> bool {
        area.width > 0 && x >= area.x && x < area.x + area.width
            && y >= area.y && y < area.y + area.height
    }

    fn tune_step(&mut self, step: i32) {
        let rx = self.app.active_rx() as u8;
        if let Some(s) = self.app.rx(rx as usize) {
            let new = (s.frequency_hz as i64 + step as i64).max(0) as u32;
            self.app.set_rx_frequency(rx, new);
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        if let Some(pos) = FOCUS_ORDER.iter().position(|f| *f == self.focus) {
            let next = if forward {
                (pos + 1) % FOCUS_ORDER.len()
            } else {
                (pos + FOCUS_ORDER.len() - 1) % FOCUS_ORDER.len()
            };
            self.focus = FOCUS_ORDER[next];
        } else {
            self.focus = Focus::Spectrum;
        }
    }

    fn cycle_agc(&mut self) {
        let rx = self.app.active_rx() as u8;
        let Some(s) = self.app.rx(rx as usize) else { return };
        let next = match s.agc_mode {
            AgcPreset::Off  => AgcPreset::Long,
            AgcPreset::Long => AgcPreset::Slow,
            AgcPreset::Slow => AgcPreset::Med,
            AgcPreset::Med  => AgcPreset::Fast,
            AgcPreset::Fast => AgcPreset::Off,
        };
        self.app.set_rx_agc(rx, next);
    }

    fn side_panel_up(&mut self) {
        let state = match self.focus {
            Focus::SideMode   => &mut self.mode_state,
            Focus::SideBand   => &mut self.band_state,
            Focus::SideFilter => &mut self.filter_state,
            Focus::SideDigital => &mut self.digital_state,
            _ => return,
        };
        let i = state.selected().unwrap_or(0);
        state.select(Some(i.saturating_sub(1)));
    }

    fn side_panel_down(&mut self) {
        let (state, max) = match self.focus {
            Focus::SideMode   => (&mut self.mode_state, 11),
            Focus::SideBand   => (&mut self.band_state, Band::ALL.len() - 1),
            Focus::SideFilter => (&mut self.filter_state, FilterPreset::ALL.len() - 1),
            Focus::SideDigital => (&mut self.digital_state, DIGITAL_OPTIONS.len() - 1),
            _ => return,
        };
        let i = state.selected().unwrap_or(0);
        state.select(Some((i + 1).min(max)));
    }

    fn side_panel_select(&mut self) {
        let rx = self.app.active_rx() as u8;
        match self.focus {
            Focus::SideMode => {
                let modes = all_modes();
                if let Some(i) = self.mode_state.selected() {
                    if let Some(&(m, _)) = modes.get(i) {
                        self.app.set_rx_mode(rx, m);
                    }
                }
            }
            Focus::SideBand => {
                if let Some(i) = self.band_state.selected() {
                    if let Some(&b) = Band::ALL.get(i) {
                        self.app.jump_to_band(b);
                    }
                }
            }
            Focus::SideFilter => {
                if let Some(i) = self.filter_state.selected() {
                    if let Some(&p) = FilterPreset::ALL.get(i) {
                        self.app.set_rx_filter_preset(rx, p);
                    }
                }
            }
            Focus::SideDigital => {
                if let Some(i) = self.digital_state.selected() {
                    if let Some(&(mode, _)) = DIGITAL_OPTIONS.get(i) {
                        self.app.set_rx_digital_mode(rx, mode);
                    }
                }
            }
            _ => {}
        }
    }

    // --- Drawing --------------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Main vertical layout
        let mut main_constraints = vec![
            Constraint::Length(1),  // menu bar
            Constraint::Length(1),  // status bar
            Constraint::Length(5),  // VFO + S-meter
        ];
        if self.repl_visible {
            main_constraints.push(Constraint::Min(10)); // spectrum + waterfall + side
            main_constraints.push(Constraint::Length(3)); // DSP controls
            main_constraints.push(Constraint::Length(7)); // REPL
        } else {
            main_constraints.push(Constraint::Min(10));
            main_constraints.push(Constraint::Length(3));
        }
        main_constraints.push(Constraint::Length(1)); // help bar

        let main_layout = Layout::vertical(main_constraints).split(area);
        let mut idx = 0;

        self.draw_menu_bar(frame, main_layout[idx]); idx += 1;
        self.draw_status_bar(frame, main_layout[idx]); idx += 1;
        self.draw_vfo_area(frame, main_layout[idx]); idx += 1;

        // Center area: spectrum+waterfall (left) + side panel (right)
        let center = main_layout[idx]; idx += 1;
        let center_split = Layout::horizontal([
            Constraint::Min(30),
            Constraint::Length(20),
        ]).split(center);
        self.draw_center(frame, center_split[0]);
        self.draw_side_panel(frame, center_split[1]);

        // DSP controls
        self.last_controls_area = main_layout[idx];
        self.draw_controls(frame, main_layout[idx]); idx += 1;

        // REPL (if visible)
        if self.repl_visible {
            self.draw_repl(frame, main_layout[idx]); idx += 1;
        }

        // Help bar
        self.draw_help_bar(frame, main_layout[idx]);

        // Popup overlays
        if let Focus::Popup(popup) = self.focus {
            self.draw_popup(frame, area, popup);
        }
    }

    fn draw_menu_bar(&self, frame: &mut Frame, area: Rect) {
        let line = Line::from(vec![
            Span::styled(" File ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(" View ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(" Help ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled("Arion-tui", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_status_bar(&self, frame: &mut Frame, area: Rect) {
        let connected = self.app.is_connected();
        let mut spans = vec![
            Span::styled(
                if connected { " ● " } else { " ○ " },
                Style::default().fg(if connected { Color::Green } else { Color::DarkGray }),
            ),
            Span::styled(self.app.radio_ip(), Style::default().fg(Color::Cyan)),
        ];
        if let Some(radio) = self.app.radio() {
            let s = radio.status();
            spans.push(Span::raw(format!(
                "  pkts {}  dsp {}k  underruns {}",
                s.session.packets_received,
                s.samples_dsp / 1000,
                s.audio_underruns,
            )));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_vfo_area(&self, frame: &mut Frame, area: Rect) {
        let num_rx = self.app.num_rx() as usize;
        // VFO columns + S-meter column
        let mut constraints: Vec<Constraint> = (0..num_rx)
            .map(|_| Constraint::Ratio(1, (num_rx + 1) as u32))
            .collect();
        constraints.push(Constraint::Ratio(1, (num_rx + 1) as u32));
        let chunks = Layout::horizontal(constraints).split(area);

        for r in 0..num_rx {
            self.draw_vfo(frame, chunks[r], r);
        }
        self.draw_smeter(frame, chunks[num_rx]);
    }

    fn draw_vfo(&self, frame: &mut Frame, area: Rect, rx: usize) {
        let Some(state) = self.app.rx(rx) else { return };
        let is_active = rx == self.app.active_rx();
        let freq = state.frequency_hz;
        let freq_str = format!("{:>2}.{:03}.{:03}", freq / 1_000_000, (freq % 1_000_000) / 1_000, freq % 1_000);
        let band = Band::for_freq(freq).map(|b| b.label()).unwrap_or("GEN");
        let mode = format!("{:?}", state.mode);

        let mut tags = vec![
            Span::styled(format!("{band} "), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{mode} "), Style::default().fg(Color::Cyan)),
        ];
        if state.nr3 { tags.push(Span::styled("NR3 ", Style::default().fg(Color::LightGreen))); }
        if state.nr4 { tags.push(Span::styled("NR4 ", Style::default().fg(Color::LightGreen))); }
        if state.locked { tags.push(Span::styled("🔒 ", Style::default().fg(Color::Yellow))); }
        tags.push(Span::styled(format!("AGC:{:?}", state.agc_mode), Style::default().fg(Color::DarkGray)));

        let lines = vec![
            Line::from(Span::styled(
                freq_str,
                Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
            )),
            Line::from(tags),
        ];

        let border_color = if is_active { Color::LightGreen } else { Color::DarkGray };
        let title = if is_active && self.app.num_rx() > 1 {
            format!("▶ RX{}", rx + 1)
        } else {
            format!("RX{}", rx + 1)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_smeter(&self, frame: &mut Frame, area: Rect) {
        let rx = self.app.active_rx();
        let dbfs = self.app.telemetry_snapshot()
            .and_then(|t| t.rx.get(rx).map(|r| r.s_meter_db))
            .unwrap_or(-140.0);
        let dbm = dbfs - SMETER_DBFS_TO_DBM_OFFSET;
        let s = dbm_to_s_units(dbm);
        let ratio = (s / 15.0).clamp(0.0, 1.0) as f64;

        let readout = if s <= 9.0 {
            format!("S{:.0}  {:+.0} dBm", s.round(), dbm)
        } else {
            format!("S9+{:.0}  {:+.0} dBm", (dbm + 73.0).max(0.0), dbm)
        };

        let color = if s > 9.0 { Color::LightRed }
            else if s > 6.0 { Color::Yellow }
            else { Color::LightGreen };

        let gauge = LineGauge::default()
            .block(Block::default().borders(Borders::ALL).title("S-Meter"))
            .ratio(ratio)
            .filled_symbol(symbols::line::THICK.horizontal)
            .unfilled_symbol(symbols::line::THICK.horizontal)
            .filled_style(Style::default().fg(color))
            .unfilled_style(Style::default().fg(Color::DarkGray))
            .label(readout);
        frame.render_widget(gauge, area);
    }

    fn draw_center(&mut self, frame: &mut Frame, area: Rect) {
        // Split: spectrum top 40%, waterfall bottom 60%
        let chunks = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ]).split(area);

        self.draw_spectrum(frame, chunks[0]);
        let rx = self.app.active_rx();
        // Resize waterfall to match terminal area
        let inner_w = chunks[1].width.saturating_sub(2) as usize;
        let inner_h = (chunks[1].height.saturating_sub(2) * 2) as usize;
        self.waterfalls[rx].resize(inner_w, inner_h);
        self.waterfalls[rx].render(frame, chunks[1]);
    }

    fn draw_spectrum(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Spectrum;
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Spectrum")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.last_spectrum_area = inner;

        let Some(snapshot) = self.app.telemetry_snapshot() else {
            frame.render_widget(
                Paragraph::new("Not connected — press 'c'").alignment(Alignment::Center),
                inner,
            );
            return;
        };

        let rx = self.app.active_rx();
        if rx >= snapshot.rx.len() { return; }
        let bins = &snapshot.rx[rx].spectrum_bins_db;
        if bins.is_empty() || inner.width == 0 { return; }

        let w = inner.width as usize;
        let mut data = vec![0u64; w];
        for (i, slot) in data.iter_mut().enumerate() {
            let src = (i * bins.len()) / w;
            let db = bins[src].clamp(-120.0, 0.0);
            *slot = ((db + 120.0) / 120.0 * inner.height as f32 * 8.0) as u64;
        }

        let sparkline = Sparkline::default()
            .data(&data)
            .max(inner.height as u64 * 8)
            .style(Style::default().fg(Color::LightGreen));
        frame.render_widget(sparkline, inner);
    }

    fn draw_side_panel(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::vertical([
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
        ]).split(area);

        self.last_mode_area = chunks[0];
        self.last_band_area = chunks[1];
        self.last_filter_area = chunks[2];
        self.last_digital_area = chunks[3];

        self.draw_mode_list(frame, chunks[0]);
        self.draw_band_list(frame, chunks[1]);
        self.draw_filter_list(frame, chunks[2]);
        self.draw_digital_list(frame, chunks[3]);
    }

    fn draw_mode_list(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::SideMode;
        let current_mode = self.app.rx(self.app.active_rx()).map(|r| r.mode).unwrap_or(WdspMode::Usb);
        let modes = all_modes();

        let items: Vec<ListItem> = modes.iter().map(|&(m, label)| {
            let style = if m == current_mode {
                Style::default().fg(Color::Black).bg(Color::LightGreen)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(label).style(style)
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Mode")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        let list = List::new(items).block(block).highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.mode_state);
    }

    fn draw_band_list(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::SideBand;
        let active_freq = self.app.rx(self.app.active_rx()).map(|v| v.frequency_hz).unwrap_or(0);
        let current_band = Band::for_freq(active_freq);

        let items: Vec<ListItem> = Band::ALL.iter().map(|&b| {
            let style = if current_band == Some(b) {
                Style::default().fg(Color::Black).bg(Color::LightGreen)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("{:>3}m", b.label())).style(style)
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Band")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        let list = List::new(items).block(block).highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.band_state);
    }

    fn draw_filter_list(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::SideFilter;
        let bw = self.app.rx(self.app.active_rx())
            .map(|r| r.filter_hi - r.filter_lo)
            .unwrap_or(2400.0);

        let items: Vec<ListItem> = FilterPreset::ALL.iter().map(|&p| {
            let is_selected = (bw - p.width_hz()).abs() < 10.0;
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::LightGreen)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(p.label()).style(style)
        }).collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Filter")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        let list = List::new(items).block(block).highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.filter_state);
    }

    fn draw_digital_list(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::SideDigital;
        let rx = self.app.active_rx() as u8;
        let current = self.app.rx_digital_mode(rx);

        // Drain new decodes from telemetry into the TUI's own ring so
        // they stay visible across frames.
        for d in self.app.rx_digital_decodes(rx) {
            let line = if d.mode == arion_core::DigitalMode::Ft8 {
                format!(
                    "[{} {:+3.0} {:.0}Hz] {}",
                    d.mode.as_str(),
                    d.snr_db,
                    d.freq_hz,
                    d.text
                )
            } else {
                format!("[{}] {}", d.mode.as_str(), d.text)
            };
            if self.digital_history.len() >= 64 {
                self.digital_history.remove(0);
            }
            self.digital_history.push(line);
        }

        // Split the panel: mode list on top, decode log below.
        let chunks = Layout::vertical([
            Constraint::Length(DIGITAL_OPTIONS.len() as u16 + 2),
            Constraint::Min(1),
        ])
        .split(area);

        let items: Vec<ListItem> = DIGITAL_OPTIONS
            .iter()
            .map(|&(m, label)| {
                let style = if m == current {
                    Style::default().fg(Color::Black).bg(Color::LightGreen)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(label).style(style)
            })
            .collect();

        let border_color = if focused { Color::Cyan } else { Color::DarkGray };
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Digital")
                    .border_style(Style::default().fg(border_color)),
            )
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, chunks[0], &mut self.digital_state);

        // Decodes log: show the most recent entries that fit.
        let log_block = Block::default()
            .borders(Borders::ALL)
            .title("Decodes")
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = log_block.inner(chunks[1]);
        frame.render_widget(log_block, chunks[1]);
        let visible = (inner.height as usize).max(1);
        let start = self.digital_history.len().saturating_sub(visible);
        let text: Vec<Line> = self.digital_history[start..]
            .iter()
            .map(|s| Line::from(s.as_str()))
            .collect();
        frame.render_widget(Paragraph::new(text), inner);
    }

    fn draw_controls(&self, frame: &mut Frame, area: Rect) {
        let rx = self.app.active_rx();
        let state = self.app.rx(rx).cloned().unwrap_or_default();
        let focused = self.focus == Focus::Controls;

        let toggle = |on: bool, label: &str| -> Span {
            if on {
                Span::styled(format!(" {label} "), Style::default().fg(Color::Black).bg(Color::LightCyan))
            } else {
                Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray))
            }
        };

        let line = Line::from(vec![
            Span::styled(if state.locked { " 🔒 " } else { " 🔓 " }, Style::default().fg(Color::Yellow)),
            Span::styled(if state.muted { " 🔇 " } else { " 🔊 " }, Style::default().fg(Color::White)),
            Span::styled(format!(" AGC:{:?} ", state.agc_mode), Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            toggle(state.nr3, "NR3"),
            toggle(state.nr4, "NR4"),
            toggle(state.anr, "ANR"),
            toggle(state.emnr, "EMNR"),
            toggle(state.nb, "NB"),
            toggle(state.nb2, "NB2"),
            toggle(state.anf, "ANF"),
            toggle(state.squelch, "SQL"),
            toggle(state.apf, "APF"),
            toggle(state.bin, "BIN"),
            toggle(state.tnf, "TNF"),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .title("DSP")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        frame.render_widget(Paragraph::new(line).block(block), area);
    }

    fn draw_repl(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Repl;
        let block = Block::default()
            .borders(Borders::ALL)
            .title("REPL (Esc to close)")
            .border_style(Style::default().fg(if focused { Color::Cyan } else { Color::DarkGray }));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
        ]).split(inner);

        // Output
        let max_lines = chunks[0].height as usize;
        let output: Vec<Line> = self.script.output()
            .iter()
            .rev()
            .take(max_lines)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| {
                let color = match l.kind {
                    arion_script::ReplLineKind::Input  => Color::DarkGray,
                    arion_script::ReplLineKind::Result => Color::LightGreen,
                    arion_script::ReplLineKind::Error  => Color::LightRed,
                    arion_script::ReplLineKind::Print  => Color::LightCyan,
                };
                Line::styled(&l.text, Style::default().fg(color))
            })
            .collect();
        frame.render_widget(Paragraph::new(output), chunks[0]);

        // Input
        let input_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::raw(&self.repl_input),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]);
        frame.render_widget(Paragraph::new(input_line), chunks[1]);
    }

    fn draw_help_bar(&self, frame: &mut Frame, area: Rect) {
        let hl = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::styled("q", hl), Span::raw(" Quit  "),
            Span::styled("c", hl), Span::raw(" Connect  "),
            Span::styled("Tab", hl), Span::raw(" Panel  "),
            Span::styled("+/-", hl), Span::raw(" Tune  "),
            Span::styled("n/N", hl), Span::raw(" NR  "),
            Span::styled("a", hl), Span::raw(" AGC  "),
            Span::styled(":", hl), Span::raw(" REPL  "),
            Span::styled("F1", hl), Span::raw(" Help  "),
            Span::styled("F5", hl), Span::raw(" Mem"),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_popup(&mut self, frame: &mut Frame, area: Rect, popup: PopupKind) {
        let popup_area = centered_rect(70, 60, area);
        frame.render_widget(Clear, popup_area);

        match popup {
            PopupKind::Help => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title("Help — Keyboard Shortcuts")
                    .border_style(Style::default().fg(Color::Cyan));
                let help_text = vec![
                    Line::raw("q / Ctrl+Q     Quit"),
                    Line::raw("c              Connect / Disconnect"),
                    Line::raw("Tab / S-Tab    Cycle panel focus"),
                    Line::raw("↑ ↓            Navigate side panel"),
                    Line::raw("Enter          Apply selection"),
                    Line::raw("+ / -          Tune ±100 Hz"),
                    Line::raw("PgUp / PgDn    Tune ±1000 Hz"),
                    Line::raw("n / N          Toggle NR3 / NR4"),
                    Line::raw("l              Toggle Lock"),
                    Line::raw("a              Cycle AGC mode"),
                    Line::raw(":              Open REPL"),
                    Line::raw("F1             This help"),
                    Line::raw("F5             Memories"),
                    Line::raw("Esc            Close popup / REPL"),
                    Line::raw(""),
                    Line::raw("Mouse: click spectrum = tune, scroll = ±100 Hz"),
                ];
                frame.render_widget(Paragraph::new(help_text).block(block), popup_area);
            }
            PopupKind::Memories => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title("Memories (Enter=load, d=delete, Esc=close)")
                    .border_style(Style::default().fg(Color::Cyan));

                let rows: Vec<Row> = self.app.memories().iter().map(|m| {
                    Row::new(vec![
                        Cell::from(m.name.clone()),
                        Cell::from(format!("{:.3}", m.freq_hz as f64 / 1e6)),
                        Cell::from(format!("{:?}", m.mode)),
                        Cell::from(m.tag.clone()),
                    ])
                }).collect();

                let widths = [
                    Constraint::Length(20),
                    Constraint::Length(12),
                    Constraint::Length(6),
                    Constraint::Length(15),
                ];
                let header = Row::new(["Name", "MHz", "Mode", "Tag"])
                    .style(Style::default().add_modifier(Modifier::BOLD));
                let table = Table::new(rows, widths)
                    .header(header)
                    .block(block)
                    .row_highlight_style(Style::default().bg(Color::DarkGray));
                frame.render_stateful_widget(table, popup_area, &mut self.mem_state);
            }
        }
    }
}

// --- Helpers -------------------------------------------------------------

fn all_modes() -> Vec<(WdspMode, &'static str)> {
    vec![
        (WdspMode::Lsb,  "LSB"),  (WdspMode::Usb,  "USB"),
        (WdspMode::CwL,  "CWL"),  (WdspMode::CwU,  "CWU"),
        (WdspMode::Am,   "AM"),   (WdspMode::Sam,  "SAM"),
        (WdspMode::Dsb,  "DSB"),  (WdspMode::Fm,   "FM"),
        (WdspMode::DigL, "DIGL"), (WdspMode::DigU, "DIGU"),
        (WdspMode::Drm,  "DRM"),  (WdspMode::Spec, "SPEC"),
    ]
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ]).split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]).split(popup_layout[1])[1]
}
