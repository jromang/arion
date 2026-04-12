//! TUI waterfall display using Canvas + HalfBlock markers.
//!
//! Each terminal cell renders 2 vertical pixels via Unicode half-block
//! characters (▀ with fg=top, bg=bottom), giving double vertical
//! resolution. Full 24-bit RGB color via `Color::Rgb`.

use ratatui::prelude::*;
use ratatui::widgets::*;

/// Scrolling waterfall buffer. Stores RGB colors per pixel.
pub struct TuiWaterfall {
    /// `pixels[row][col]` — row 0 = newest (top of waterfall).
    pixels: Vec<Vec<Color>>,
    width: usize,
    height: usize, // in pixels (2× terminal rows)
}

impl TuiWaterfall {
    pub fn new(width: usize, height: usize) -> Self {
        TuiWaterfall {
            pixels: vec![vec![Color::Black; width]; height],
            width,
            height,
        }
    }

    /// Resize to match terminal area. Clears the buffer.
    pub fn resize(&mut self, width: usize, height_pixels: usize) {
        if width != self.width || height_pixels != self.height {
            self.width = width;
            self.height = height_pixels;
            self.pixels = vec![vec![Color::Black; width]; height_pixels];
        }
    }

    /// Push a new spectrum row (newest at top). Shifts all existing
    /// rows down by one, dropping the oldest.
    pub fn push_row(&mut self, bins_db: &[f32]) {
        if self.height == 0 || self.width == 0 {
            return;
        }
        // Shift rows down
        self.pixels.pop();
        let mut new_row = vec![Color::Black; self.width];
        for (i, px) in new_row.iter_mut().enumerate() {
            let src = (i * bins_db.len()) / self.width.max(1);
            let src = src.min(bins_db.len().saturating_sub(1));
            *px = db_to_color(bins_db[src]);
        }
        self.pixels.insert(0, new_row);
    }

    /// Render the waterfall into a terminal Rect using HalfBlock
    /// characters. Each terminal row = 2 pixel rows.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Waterfall")
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Each terminal row = 2 pixel rows via ▀ (fg=top, bg=bottom)
        let term_rows = inner.height as usize;
        let term_cols = inner.width as usize;

        let mut lines: Vec<Line> = Vec::with_capacity(term_rows);
        for tr in 0..term_rows {
            let top_row = tr * 2;
            let bot_row = tr * 2 + 1;
            let mut spans: Vec<Span> = Vec::with_capacity(term_cols);
            for tc in 0..term_cols {
                let col_idx = (tc * self.width) / term_cols.max(1);
                let col_idx = col_idx.min(self.width.saturating_sub(1));
                let fg = self.pixels.get(top_row)
                    .and_then(|r| r.get(col_idx))
                    .copied()
                    .unwrap_or(Color::Black);
                let bg = self.pixels.get(bot_row)
                    .and_then(|r| r.get(col_idx))
                    .copied()
                    .unwrap_or(Color::Black);
                spans.push(Span::styled("▀", Style::default().fg(fg).bg(bg)));
            }
            lines.push(Line::from(spans));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }
}

/// Map dBFS to an RGB color. Same palette as the egui waterfall:
/// black → blue → cyan → yellow → red.
fn db_to_color(db: f32) -> Color {
    let t = ((db + 120.0) / 120.0).clamp(0.0, 1.0);
    let (r, g, b) = if t < 0.25 {
        let v = (t / 0.25 * 255.0) as u8;
        (0, 0, v)
    } else if t < 0.5 {
        let v = ((t - 0.25) / 0.25 * 255.0) as u8;
        (0, v, 255)
    } else if t < 0.75 {
        let v = ((t - 0.5) / 0.25 * 255.0) as u8;
        (v, 255, 255 - v)
    } else {
        let v = 255 - ((t - 0.75) / 0.25 * 255.0) as u8;
        (255, v, 0)
    };
    Color::Rgb(r, g, b)
}
