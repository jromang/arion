//! ratatui console frontend for Thetis.
//!
//! Consumes the same [`thetis_app::App`] as the egui frontend. All
//! state lives in App — this crate is a pure renderer + input
//! dispatcher. Zero dependency on any graphics library.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::*;

use thetis_app::{App, AppOptions, Band, dbm_to_s_units, SMETER_DBFS_TO_DBM_OFFSET};
use thetis_script::ScriptEngine;

pub struct TuiView {
    app: App,
    script: ScriptEngine,
    repl_input: String,
    repl_visible: bool,
    focus: Focus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Spectrum,
    Repl,
}

impl TuiView {
    pub fn new(opts: AppOptions) -> Self {
        TuiView {
            app: App::new(opts),
            script: ScriptEngine::default(),
            repl_input: String::new(),
            repl_visible: false,
            focus: Focus::Spectrum,
        }
    }

    /// Main event loop. Runs until the user quits.
    pub fn run(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

        let tick_rate = Duration::from_millis(50); // 20 fps

        loop {
            self.app.tick(Instant::now());
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(tick_rate)? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key) {
                        break;
                    }
                }
            }
        }

        disable_raw_mode()?;
        terminal.backend_mut().execute(LeaveAlternateScreen)?;
        self.app.shutdown();
        Ok(())
    }

    /// Returns true if the app should quit.
    fn handle_key(&mut self, key: event::KeyEvent) -> bool {
        // Global shortcuts
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('q')) if self.focus != Focus::Repl => return true,
            (KeyModifiers::CONTROL, KeyCode::Char('q')) => return true,
            (KeyModifiers::NONE, KeyCode::Char(':')) if self.focus != Focus::Repl => {
                self.repl_visible = true;
                self.focus = Focus::Repl;
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Char('c')) if self.focus != Focus::Repl => {
                if self.app.is_connected() {
                    self.app.disconnect();
                } else {
                    self.app.connect();
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Char('n')) if self.focus != Focus::Repl => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.nr3).unwrap_or(false);
                self.app.set_rx_nr3(rx, on);
                return false;
            }
            (KeyModifiers::SHIFT, KeyCode::Char('N')) if self.focus != Focus::Repl => {
                let rx = self.app.active_rx() as u8;
                let on = self.app.rx(rx as usize).map(|r| !r.nr4).unwrap_or(false);
                self.app.set_rx_nr4(rx, on);
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Char('+')) if self.focus != Focus::Repl => {
                let rx = self.app.active_rx() as u8;
                if let Some(s) = self.app.rx(rx as usize) {
                    self.app.set_rx_frequency(rx, s.frequency_hz.saturating_add(100));
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Char('-')) if self.focus != Focus::Repl => {
                let rx = self.app.active_rx() as u8;
                if let Some(s) = self.app.rx(rx as usize) {
                    self.app.set_rx_frequency(rx, s.frequency_hz.saturating_sub(100));
                }
                return false;
            }
            (_, KeyCode::Esc) => {
                if self.focus == Focus::Repl {
                    self.focus = Focus::Spectrum;
                    return false;
                }
            }
            _ => {}
        }

        // REPL input handling
        if self.focus == Focus::Repl {
            match key.code {
                KeyCode::Enter => {
                    let line = self.repl_input.clone();
                    self.repl_input.clear();
                    self.script.run_line(&line, &mut self.app);
                    self.script.apply_pending_commands(&mut self.app);
                }
                KeyCode::Char(c) => {
                    self.repl_input.push(c);
                }
                KeyCode::Backspace => {
                    self.repl_input.pop();
                }
                _ => {}
            }
        }

        false
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();

        // Layout: status bar top, REPL bottom (if visible), spectrum center
        let mut constraints = vec![
            Constraint::Length(3), // status bar
            Constraint::Length(5), // VFO display
        ];
        if self.repl_visible {
            constraints.push(Constraint::Min(8));    // spectrum
            constraints.push(Constraint::Length(8));  // REPL
        } else {
            constraints.push(Constraint::Min(8));    // spectrum
        }
        constraints.push(Constraint::Length(1)); // help bar

        let layout = Layout::vertical(constraints).split(area);
        let mut idx = 0;

        // --- Status bar ---
        self.draw_status_bar(frame, layout[idx]);
        idx += 1;

        // --- VFO display ---
        self.draw_vfo(frame, layout[idx]);
        idx += 1;

        // --- Spectrum ---
        self.draw_spectrum(frame, layout[idx]);
        idx += 1;

        // --- REPL (if visible) ---
        if self.repl_visible {
            self.draw_repl(frame, layout[idx]);
            idx += 1;
        }

        // --- Help bar ---
        self.draw_help_bar(frame, layout[idx]);
    }

    fn draw_status_bar(&self, frame: &mut Frame, area: Rect) {
        let connected = self.app.is_connected();
        let dot = if connected { "● Connected" } else { "○ Disconnected" };
        let dot_style = if connected {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let mut spans = vec![
            Span::styled(dot, dot_style),
            Span::raw("  "),
            Span::styled(
                format!("IP: {}", self.app.radio_ip()),
                Style::default().fg(Color::Cyan),
            ),
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

        let line = Line::from(spans);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Thetis-tui")
            .border_style(Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(line).block(block);
        frame.render_widget(paragraph, area);
    }

    fn draw_vfo(&self, frame: &mut Frame, area: Rect) {
        let num_rx = self.app.num_rx() as usize;
        let chunks = Layout::horizontal(
            (0..num_rx).map(|_| Constraint::Ratio(1, num_rx as u32))
        ).split(area);

        for r in 0..num_rx {
            let Some(state) = self.app.rx(r) else { continue };
            let is_active = r == self.app.active_rx();
            let prefix = if is_active && num_rx > 1 { "▶ " } else { "" };

            let freq = state.frequency_hz;
            let freq_str = format!(
                "{:>2}.{:03}.{:03}",
                freq / 1_000_000,
                (freq % 1_000_000) / 1_000,
                freq % 1_000,
            );

            let band = Band::for_freq(freq).map(|b| b.label()).unwrap_or("GEN");
            let mode = format!("{:?}", state.mode);

            // S-meter from telemetry
            let smeter = self.app.telemetry_snapshot()
                .and_then(|t| t.rx.get(r).map(|rx| rx.s_meter_db))
                .unwrap_or(-140.0);
            let dbm = smeter - SMETER_DBFS_TO_DBM_OFFSET;
            let s = dbm_to_s_units(dbm);
            let s_str = if s <= 9.0 {
                format!("S{:.0}", s.round())
            } else {
                format!("S9+{:.0}", (dbm + 73.0).max(0.0))
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!("{prefix}RX{} ", r + 1),
                        Style::default().fg(if is_active { Color::LightGreen } else { Color::DarkGray }),
                    ),
                    Span::styled(
                        freq_str,
                        Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(format!("{band} {mode}"), Style::default().fg(Color::Yellow)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{s_str} {dbm:+.0} dBm"),
                        Style::default().fg(if s > 7.0 { Color::LightRed } else { Color::LightGreen }),
                    ),
                    if state.nr3 { Span::styled(" NR3", Style::default().fg(Color::Cyan)) } else { Span::raw("") },
                    if state.nr4 { Span::styled(" NR4", Style::default().fg(Color::Cyan)) } else { Span::raw("") },
                ]),
            ];

            let title = format!("RX{}", r + 1);
            let border_color = if is_active { Color::LightGreen } else { Color::DarkGray };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(border_color));
            let paragraph = Paragraph::new(lines).block(block);
            frame.render_widget(paragraph, chunks[r]);
        }
    }

    fn draw_spectrum(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Spectrum")
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(snapshot) = self.app.telemetry_snapshot() else {
            let msg = Paragraph::new("Not connected — press 'c' to connect")
                .alignment(Alignment::Center);
            frame.render_widget(msg, inner);
            return;
        };

        let rx = self.app.active_rx();
        if rx >= snapshot.rx.len() {
            return;
        }
        let bins = &snapshot.rx[rx].spectrum_bins_db;
        if bins.is_empty() || inner.width == 0 {
            return;
        }

        // Downsample bins to terminal width
        let w = inner.width as usize;
        let mut display_bins = vec![0u64; w];
        for (i, slot) in display_bins.iter_mut().enumerate() {
            let src = (i * bins.len()) / w;
            let db = bins[src].clamp(-120.0, 0.0);
            let norm = ((db + 120.0) / 120.0 * inner.height as f32 * 8.0) as u64;
            *slot = norm;
        }

        let sparkline = Sparkline::default()
            .data(&display_bins)
            .max(inner.height as u64 * 8)
            .style(Style::default().fg(Color::LightGreen));
        frame.render_widget(sparkline, inner);
    }

    fn draw_repl(&self, frame: &mut Frame, area: Rect) {
        let focus_color = if self.focus == Focus::Repl { Color::Cyan } else { Color::DarkGray };
        let block = Block::default()
            .borders(Borders::ALL)
            .title("REPL (Esc to close, Enter to run)")
            .border_style(Style::default().fg(focus_color));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Split: output lines + input line at bottom
        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
        ]).split(inner);

        // Output
        let output_lines: Vec<Line> = self.script.output()
            .iter()
            .rev()
            .take(chunks[0].height as usize)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| {
                let color = match l.kind {
                    thetis_script::ReplLineKind::Input  => Color::DarkGray,
                    thetis_script::ReplLineKind::Result => Color::LightGreen,
                    thetis_script::ReplLineKind::Error  => Color::LightRed,
                    thetis_script::ReplLineKind::Print  => Color::LightCyan,
                };
                Line::styled(&l.text, Style::default().fg(color))
            })
            .collect();
        let output = Paragraph::new(output_lines);
        frame.render_widget(output, chunks[0]);

        // Input line
        let input_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::raw(&self.repl_input),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]);
        let input = Paragraph::new(input_line);
        frame.render_widget(input, chunks[1]);
    }

    fn draw_help_bar(&self, frame: &mut Frame, area: Rect) {
        let help = Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Quit  "),
            Span::styled("c", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Connect  "),
            Span::styled("+/-", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Tune  "),
            Span::styled("n/N", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" NR3/4  "),
            Span::styled(":", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" REPL  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Back"),
        ]);
        frame.render_widget(Paragraph::new(help), area);
    }
}
