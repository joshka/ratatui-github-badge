//! The Ratatui pixel wordmark (assets/ratatui-wordmark.svg) drawn with half-block cells.
use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

include!(concat!(env!("OUT_DIR"), "/wordmark.rs"));

/// The drop-shadow gray from the SVG.
pub const SHADOW: Color = Color::Rgb(0x47, 0x47, 0x47);

pub struct Wordmark {
    pub letters: Color,
    pub shadow: Color,
}

impl Wordmark {
    /// 36 columns by 3 rows: one pixel per column, two per row.
    pub const SIZE: (u16, u16) = (36, 3);
}

impl Widget for Wordmark {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pixel = |x: usize, y: usize| match WORDMARK.get(y).map_or(0, |row| row[x]) {
            1 => Some(self.letters),
            2 => Some(self.shadow),
            _ => None,
        };
        let area = area.intersection(Rect::new(area.x, area.y, Self::SIZE.0, Self::SIZE.1));
        for row in 0..area.height {
            for col in 0..area.width {
                let (x, y) = (col as usize, row as usize * 2);
                let cell = &mut buf[(area.x + col, area.y + row)];
                match (pixel(x, y), pixel(x, y + 1)) {
                    (None, None) => {}
                    (Some(top), None) => {
                        cell.set_symbol("▀").set_fg(top);
                    }
                    (None, Some(bottom)) => {
                        cell.set_symbol("▄").set_fg(bottom);
                    }
                    (Some(top), Some(bottom)) => {
                        cell.set_symbol("▀").set_fg(top).set_bg(bottom);
                    }
                }
            }
        }
    }
}
