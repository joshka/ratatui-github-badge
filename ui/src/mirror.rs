//! Render a Ratatui buffer as text for the USB serial mirror.
use core::fmt::Write;

use alloc::string::String;
use ratatui::{buffer::Buffer, buffer::CellDiffOption, style::Color};

/// Starts a mirror frame; log lines can arrive between frames but never inside one.
pub const FRAME_START: char = '\x02';
/// Ends a mirror frame.
pub const FRAME_END: char = '\x03';

/// A worst-case row: every cell changes both colors (about 19 bytes each) plus its symbol.
const MAX_ROW: usize = 2 * 1024 + 512;

/// One true-color ANSI frame between [`FRAME_START`] and [`FRAME_END`], only emitting
/// colors when they change. Busy pages can need tens of KB, so give up (returning false, with
/// `out` empty) rather than grow `out` past `max` bytes: on the badge, a doubling `String` can
/// ask for a block bigger than the heap has free.
pub fn ansi(buf: &Buffer, out: &mut String, max: usize) -> bool {
    out.clear();
    out.push(FRAME_START);
    for y in 0..buf.area.height {
        if out.len() + MAX_ROW > max {
            out.clear();
            return false;
        }
        let (mut fg, mut bg) = (None, None);
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if fg != Some(cell.fg) {
                sgr(out, cell.fg, 30);
                fg = Some(cell.fg);
            }
            if bg != Some(cell.bg) {
                sgr(out, cell.bg, 40);
                bg = Some(cell.bg);
            }
            out.push_str(symbol(cell));
        }
        out.push_str("\x1b[0m\r\n");
    }
    out.push(FRAME_END);
    true
}

/// Plain text, one line per row.
pub fn text(buf: &Buffer, out: &mut String) {
    out.clear();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(symbol(&buf[(x, y)]));
        }
        out.push_str("\r\n");
    }
}

fn symbol(cell: &ratatui::buffer::Cell) -> &str {
    // Image slots are drawn as pixels on the badge; show them as blank here.
    if cell.diff_option == CellDiffOption::Skip { " " } else { cell.symbol() }
}

/// `base` is 30 for foreground, 40 for background.
fn sgr(out: &mut String, color: Color, base: u8) {
    let _ = match color {
        Color::Reset => write!(out, "\x1b[{}m", base + 9),
        Color::Rgb(r, g, b) => write!(out, "\x1b[{};2;{r};{g};{b}m", base + 8),
        Color::Indexed(i) => write!(out, "\x1b[{};5;{i}m", base + 8),
        named => {
            let (offset, bright) = match named {
                Color::Black => (0, false),
                Color::Red => (1, false),
                Color::Green => (2, false),
                Color::Yellow => (3, false),
                Color::Blue => (4, false),
                Color::Magenta => (5, false),
                Color::Cyan => (6, false),
                Color::Gray => (7, false),
                Color::DarkGray => (0, true),
                Color::LightRed => (1, true),
                Color::LightGreen => (2, true),
                Color::LightYellow => (3, true),
                Color::LightBlue => (4, true),
                Color::LightMagenta => (5, true),
                Color::LightCyan => (6, true),
                _ => (7, true),
            };
            write!(out, "\x1b[{}m", base + offset + if bright { 60 } else { 0 })
        }
    };
}
