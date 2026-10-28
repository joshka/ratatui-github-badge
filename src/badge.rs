//! Conference identity, offline contact sharing, and a session countdown.
use crate::timer::SessionTimer;
use embedded_graphics::{
    image::{Image, ImageRawBE},
    pixelcolor::Rgb565,
    prelude::*,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Paragraph},
};

const BG: Color = Color::Rgb(8, 12, 24);
const ACCENTS: [Color; 4] = [
    Color::Rgb(80, 240, 200),
    Color::Rgb(255, 120, 200),
    Color::Rgb(160, 160, 255),
    Color::Rgb(255, 210, 100),
];
const MOODS: [&str; 4] = [
    "OPEN TO CHAT",
    "ASK ME ABOUT RUST",
    "LET'S BUILD A TUI",
    "IN A SESSION / SAY HI LATER",
];
const PAGES: [&str; 7] = [
    "BADGE",
    "SHARE CONTACT",
    "SESSION TIMER",
    "RATATUI",
    "PLAYGROUND",
    "WI-FI",
    "DIAGNOSTICS",
];
const CONTACT_LABELS: [&str; 3] = ["SAVE CONTACT", "GITHUB / @ferris", "LINKEDIN / ferris"];
const CONTACT_CODES: [&[u8]; 3] = [
    include_bytes!("../assets/contact.rgb565"),
    include_bytes!("../assets/github.rgb565"),
    include_bytes!("../assets/linkedin.rgb565"),
];

#[derive(Clone, Copy, Default)]
pub struct Badge {
    pub page: usize,
    pub tick: u32,
    pub timer: SessionTimer,
    status: usize,
    contact: usize,
    theme: usize,
    paused: bool,
}

impl Badge {
    pub fn navigate(&mut self, forward: bool) {
        self.page = (self.page + if forward { 1 } else { PAGES.len() - 1 }) % PAGES.len();
    }

    pub fn adjust(&mut self, forward: bool) {
        let offset = if forward { 1 } else { 3 };
        match self.page {
            0 => self.status = (self.status + offset) % MOODS.len(),
            1 => self.contact = (self.contact + if forward { 1 } else { 2 }) % 3,
            2 => self.timer.set_preset(forward),
            3 | 4 => self.theme = (self.theme + offset) % ACCENTS.len(),
            _ => {}
        }
    }

    pub fn activate(&mut self) {
        match self.page {
            0 | 3 => self.theme = (self.theme + 1) % ACCENTS.len(),
            1 => self.contact = (self.contact + 1) % 3,
            2 => self.timer.toggle(),
            4 => self.paused = !self.paused,
            5 => crate::wifi::request_start(),
            _ => {}
        }
    }

    pub fn advance(&mut self, elapsed_ms: u64) {
        self.timer.advance(elapsed_ms);
        if !self.paused {
            self.tick = self.tick.wrapping_add(1);
        }
    }

    pub fn draw(self, f: &mut Frame) {
        let accent = ACCENTS[self.theme];
        f.render_widget(
            Block::new().style(Style::new().bg(BG).fg(Color::White)),
            f.area(),
        );
        self.line(
            f,
            0,
            &alloc::format!("UNIVERSE '26   /   {}", PAGES[self.page]),
            accent,
        );
        // Rows 2..6 hold the official logo. QR pages reserve rows 7..19.
        match self.page {
            0 => {
                f.render_widget(
                    Paragraph::new("Ferris the Crab")
                        .centered()
                        .style(Color::White),
                    Rect::new(19, 13, 32, 1),
                );
                f.render_widget(
                    Paragraph::new("RATATUI MAINTAINER")
                        .centered()
                        .style(accent),
                    Rect::new(19, 15, 32, 1),
                );
                self.line(f, 19, MOODS[self.status], accent);

                self.line(f, 20, "UP/DOWN status   ENTER color", accent);
            }
            1 => {
                self.line(f, 20, CONTACT_LABELS[self.contact], accent);
                self.line(f, 21, "Scan with phone  /  UP/DOWN choose", Color::White);
            }
            2 => self.draw_timer(f, accent),
            3 => {
                self.line(f, 20, "RATATUI.RS / BUILD YOUR NEXT TUI", accent);
                self.line(f, 21, "Scan for docs, examples + community", Color::White);
            }
            4 => self.draw_playground(f, accent),
            5 => self.draw_wifi(f, accent),
            _ => self.draw_diagnostics(f, accent),
        }
        let footer = match self.page {
            0 | 3 => "LEFT prev      ENTER color      RIGHT next",
            1 => "LEFT prev      ENTER next QR    RIGHT next",
            2 => "LEFT prev   ENTER start/pause   RIGHT next",
            4 => "LEFT prev      ENTER pause      RIGHT next",
            5 => "LEFT prev    ENTER connect      RIGHT next",
            _ => "LEFT prev                       RIGHT next",
        };
        self.line(f, 23, footer, accent);
    }

    fn draw_timer(self, f: &mut Frame, accent: Color) {
        let seconds = self.timer.remaining_ms.div_ceil(1000);
        let time = alloc::format!("{:02}:{:02}", seconds / 60, seconds % 60);
        const DIGITS: [[&str; 5]; 11] = [
            ["###", "# #", "# #", "# #", "###"],
            [" ##", "  #", "  #", "  #", "  #"],
            ["###", "  #", "###", "#  ", "###"],
            ["###", "  #", "###", "  #", "###"],
            ["# #", "# #", "###", "  #", "  #"],
            ["###", "#  ", "###", "  #", "###"],
            ["###", "#  ", "###", "# #", "###"],
            ["###", "  #", "  #", "  #", "  #"],
            ["###", "# #", "###", "# #", "###"],
            ["###", "# #", "###", "  #", "###"],
            ["   ", " # ", "   ", " # ", "   "],
        ];
        let mut lines: [alloc::string::String; 5] =
            core::array::from_fn(|_| alloc::string::String::new());
        for c in time.bytes() {
            let digit = if c == b':' { 10 } else { (c - b'0') as usize };
            for (line, pattern) in lines.iter_mut().zip(DIGITS[digit]) {
                for cell in pattern.bytes() {
                    line.push_str(if cell == b'#' { "██" } else { "  " });
                }
                line.push_str("  ");
            }
        }
        for (row, line) in lines.iter().enumerate() {
            self.line(f, 9 + row as u16, line, accent);
        }
        let status = if seconds == 0 {
            "SESSION COMPLETE"
        } else if self.timer.running {
            "IN SESSION"
        } else {
            "READY / PAUSED"
        };
        self.line(f, 16, status, Color::White);
        self.line(f, 18, "UP/DOWN: reset to 15 / 30 / 45 / 60 min", accent);
        self.line(
            f,
            20,
            "Keeps counting while you browse pages.",
            Color::White,
        );
        self.line(f, 21, "Resets when badge powers off.", Color::Gray);
    }

    fn draw_playground(self, f: &mut Frame, accent: Color) {
        self.line(
            f,
            8,
            if self.paused {
                "STARFIELD / PAUSED"
            } else {
                "STARFIELD / LIVE"
            },
            accent,
        );
        let bounds = Rect::new(3, 10, f.area().width.saturating_sub(6), 7);
        f.render_widget(Block::bordered().border_style(accent), bounds);
        for i in 0..18u32 {
            let x = bounds.x
                + 1
                + ((i * 17 + self.tick / (1 + self.theme as u32))
                    % u32::from(bounds.width.saturating_sub(2).max(1))) as u16;
            let y = bounds.y + 1 + ((i * 7 + self.tick / 5) % 5) as u16;
            f.render_widget(
                Paragraph::new(if i % 3 == 0 { "+" } else { "*" }).style(accent),
                Rect::new(x, y, 1, 1),
            );
        }
        self.line(f, 19, "ENTER pause/resume   UP/DOWN color", Color::White);
        self.line(f, 21, "Ratatui + Mousefood / running on RP2350", accent);
    }

    fn draw_wifi(self, f: &mut Frame, accent: Color) {
        let status = crate::wifi::status();
        self.line(f, 8, "CONFERENCE WI-FI", accent);
        self.line(f, 11, status.phase, Color::White);
        let ssid = core::str::from_utf8(&status.ssid[..status.ssid_len]).unwrap_or("Unknown");
        self.line(f, 13, ssid, accent);
        if status.ip != [0; 4] {
            let [a, b, c, d] = status.ip;
            self.line(f, 15, &alloc::format!("IP {a}.{b}.{c}.{d}"), Color::White);
        }
        self.line(
            f,
            17,
            &alloc::format!("{} access points seen", status.found),
            Color::Gray,
        );
        self.line(f, 20, "Contacts + profile work offline.", Color::White);
        self.line(f, 21, "Internet / captive portal not checked.", Color::Gray);
    }

    fn draw_diagnostics(self, f: &mut Frame, accent: Color) {
        self.line(f, 8, "LAST FAILURE", accent);
        if let Some(report) = crate::recovery::report() {
            let text = report.as_str();
            f.render_widget(
                Paragraph::new(text)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .style(Color::White),
                Rect::new(2, 10, f.area().width.saturating_sub(4), 9),
            );
        } else {
            self.line(f, 12, "No failure recorded this boot.", Color::White);
        }
        let (used, free) = crate::heap_usage();
        self.line(
            f,
            20,
            &alloc::format!("Heap: {}K used / {}K free", used / 1024, free / 1024),
            accent,
        );
        self.line(f, 21, "Hold LEFT + ENTER: bootloader", Color::Gray);
    }

    fn line(self, f: &mut Frame, y: u16, text: &str, color: Color) {
        f.render_widget(
            Paragraph::new(text).centered().style(color),
            Rect::new(0, y, f.area().width, 1),
        );
    }

    pub fn overlay(self, display: &mut crate::lcd::Lcd) {
        let logo = ImageRawBE::<Rgb565>::new(include_bytes!("../assets/ratatui.rgb565"), 296);
        Image::new(&logo, Point::new(12, 25)).draw(display).unwrap();
        match self.page {
            0 => {
                let handle =
                    ImageRawBE::<Rgb565>::new(include_bytes!("../assets/handle.rgb565"), 200);
                Image::new(&handle, Point::new(108, 78))
                    .draw(display)
                    .unwrap();
                let avatar =
                    ImageRawBE::<Rgb565>::new(include_bytes!("../assets/avatar.rgb565"), 88);
                Image::new(&avatar, Point::new(12, 76))
                    .draw(display)
                    .unwrap();
            }
            1 | 3 => {
                let data = if self.page == 1 {
                    CONTACT_CODES[self.contact]
                } else {
                    include_bytes!("../assets/project-qr.rgb565").as_slice()
                };
                let qr = ImageRawBE::<Rgb565>::new(data, 132);
                Image::new(&qr, Point::new(94, 66)).draw(display).unwrap();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_wraps_in_both_directions() {
        let mut badge = Badge::default();
        badge.navigate(false);
        assert_eq!(badge.page, PAGES.len() - 1);
        badge.navigate(true);
        assert_eq!(badge.page, 0);
        for _ in 0..PAGES.len() {
            badge.navigate(true);
        }
        assert_eq!(badge.page, 0);
    }

    #[test]
    fn contact_selection_wraps_without_leaving_asset_bounds() {
        let mut badge = Badge {
            page: 1,
            ..Default::default()
        };
        badge.adjust(false);
        assert_eq!(badge.contact, 2);
        for _ in 0..100 {
            badge.activate();
            assert!(badge.contact < CONTACT_CODES.len());
            assert_eq!(CONTACT_CODES[badge.contact].len(), 132 * 132 * 2);
        }
    }
}
