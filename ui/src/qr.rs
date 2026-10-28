//! QR codes drawn with half-block cells, two modules per terminal row (like tui-qrcode).
use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

include!(concat!(env!("OUT_DIR"), "/qr_codes.rs"));

const QUIET: usize = 2;
const DARK: Color = Color::Rgb(0, 0, 0);
const LIGHT: Color = Color::Rgb(255, 255, 255);

pub struct Qr {
    pub width: usize,
    pub modules: &'static [bool],
}

impl Qr {
    /// Size in cells including a two-module quiet zone.
    pub const fn size(&self) -> (u16, u16) {
        let side = self.width + 2 * QUIET;
        (side as u16, side.div_ceil(2) as u16)
    }

    fn dark(&self, x: usize, y: usize) -> bool {
        let (x, y) = (x.wrapping_sub(QUIET), y.wrapping_sub(QUIET));
        x < self.width && y < self.width && self.modules[y * self.width + x]
    }
}

impl Widget for &Qr {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (w, h) = self.size();
        let area = area.intersection(Rect::new(area.x, area.y, w, h));
        for row in 0..area.height {
            for col in 0..area.width {
                let (x, y) = (col as usize, row as usize * 2);
                let color = |dark| if dark { DARK } else { LIGHT };
                buf[(area.x + col, area.y + row)]
                    .set_symbol("▀")
                    .set_fg(color(self.dark(x, y)))
                    .set_bg(color(self.dark(x, y + 1)));
            }
        }
    }
}
