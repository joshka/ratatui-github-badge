//! Big, high-contrast things for the booth demo: a full-width wordmark, a scrolling
//! marquee, Doom-style fire and Conway's Life. All drawn as Ratatui cells.
use alloc::{vec, vec::Vec};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use tui_big_text::{BigText, PixelSize};

use crate::wordmark::WORDMARK;

const QUADRANTS: [&str; 16] = [
    " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█",
];

/// Draw a pixel function with quadrant characters: each cell is 2x2 sub-pixels and each
/// source pixel covers `scale` sub-pixels. Cells can only hold two colors, so when a cell
/// mixes more, the lower-priority (later) color is dropped.
pub fn quadrants(buf: &mut Buffer, area: Rect, scale: (usize, usize), pixel: impl Fn(usize, usize) -> Option<(u8, Color)>) {
    let area = area.intersection(buf.area);
    for row in 0..area.height as usize {
        for col in 0..area.width as usize {
            let sub = |dx: usize, dy: usize| pixel((col * 2 + dx) / scale.0, (row * 2 + dy) / scale.1);
            let quads = [sub(0, 0), sub(1, 0), sub(0, 1), sub(1, 1)];
            let mut colors: Vec<(u8, Color)> = quads.iter().flatten().copied().collect();
            if colors.is_empty() {
                continue;
            }
            colors.sort_by_key(|(priority, _)| *priority);
            colors.dedup();
            let fg = colors[0];
            let other = colors.get(1).copied();
            let full = quads.iter().all(Option::is_some);
            let mut mask = 0;
            for (i, q) in quads.iter().enumerate() {
                if *q == Some(fg) {
                    mask |= 1 << i;
                }
            }
            let cell = &mut buf[(area.x + col as u16, area.y + row as u16)];
            cell.set_symbol(QUADRANTS[mask]).set_fg(fg.1);
            if let (true, Some(bg)) = (full, other) {
                cell.set_bg(bg.1);
            }
        }
    }
}

/// The wordmark at 9x10 px per pixel: 53x5 cells, the whole screen width.
pub struct BigWordmark {
    pub letters: Color,
    pub shadow: Color,
}

impl BigWordmark {
    pub const HEIGHT: u16 = 5;
}

impl Widget for BigWordmark {
    fn render(self, area: Rect, buf: &mut Buffer) {
        quadrants(buf, area, (3, 2), |x, y| match WORDMARK.get(y).and_then(|r| r.get(x)) {
            Some(1) => Some((0, self.letters)),
            Some(2) => Some((1, self.shadow)),
            _ => None,
        });
    }
}

/// Full-size `tui-big-text` scrolling right to left; only the visible glyphs are rendered.
pub fn marquee(buf: &mut Buffer, area: Rect, text: &str, offset_cols: usize, color: impl Fn(u16) -> Color) {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len() * 8;
    let offset = offset_cols % total;
    let first = offset / 8;
    let visible = area.width as usize / 8 + 2;
    let window: alloc::string::String = (0..visible).map(|i| chars[(first + i) % chars.len()]).collect();
    let mut scratch = Buffer::empty(Rect::new(0, 0, (visible * 8) as u16, 8));
    BigText::builder()
        .pixel_size(PixelSize::Full)
        .lines(vec![window.into()])
        .build()
        .render(scratch.area, &mut scratch);
    let shift = (offset % 8) as u16;
    for y in 0..area.height.min(8) {
        for x in 0..area.width {
            let src = &scratch[(x + shift, y)];
            if src.symbol() != " " {
                buf[(area.x + x, area.y + y)].set_symbol(src.symbol()).set_style(Style::new().fg(color(x)));
            }
        }
    }
}

/// Tiny xorshift so visuals don't need a crate.
pub struct Rng(pub u32);

impl Rng {
    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// The classic Doom fire: heat rises from the bottom row, cooling and drifting randomly.
pub struct Fire {
    width: usize,
    height: usize,
    heat: Vec<u8>,
}

const FIRE_MAX: u8 = 36;

impl Fire {
    pub fn new(width: usize, height: usize) -> Self {
        let mut heat = vec![0; width * height];
        heat[(height - 1) * width..].fill(FIRE_MAX);
        Self { width, height, heat }
    }

    pub fn step(&mut self, rng: &mut Rng) {
        for x in 0..self.width {
            for y in 1..self.height {
                let r = rng.next_u32();
                let src = self.heat[y * self.width + x];
                let drift = (r & 3) as usize;
                let dst_x = (x + self.width + 1 - drift.min(2)) % self.width;
                // Only ~18 rows tall, so cool faster than the original's 0-1 per row.
                let cooled = src.saturating_sub(((r >> 2) % 5) as u8);
                self.heat[(y - 1) * self.width + dst_x] = cooled;
            }
        }
    }
}

impl Widget for &Fire {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = |h: u8| fire_color(h);
        let area = area.intersection(buf.area);
        for row in 0..area.height as usize {
            for col in 0..(area.width as usize).min(self.width) {
                let top = self.heat.get(row * 2 * self.width + col).copied().unwrap_or(0);
                let bottom = self.heat.get((row * 2 + 1) * self.width + col).copied().unwrap_or(0);
                if top == 0 && bottom == 0 {
                    continue;
                }
                buf[(area.x + col as u16, area.y + row as u16)]
                    .set_symbol("▀")
                    .set_fg(color(top))
                    .set_bg(color(bottom));
            }
        }
    }
}

fn fire_color(h: u8) -> Color {
    let t = h as u32 * 255 / FIRE_MAX as u32;
    let (r, g, b) = match t {
        0..=84 => (t * 3, 0, t / 2),
        85..=169 => (255, (t - 85) * 3, 0),
        _ => (255, 255, (t - 170) * 3),
    };
    Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8)
}

/// Conway's Life on a wrapping half-block grid, cells remembering their age for color.
pub struct Life {
    pub width: usize,
    pub height: usize,
    age: Vec<u8>,
}

impl Life {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, age: vec![0; width * height] }
    }

    /// Seed with the wordmark letters (2 rows per pixel) plus a scatter of noise.
    pub fn seed(&mut self, rng: &mut Rng) {
        self.age.fill(0);
        let (ox, oy) = ((self.width - 36) / 2, (self.height - 10) / 2);
        for (y, row) in WORDMARK.iter().enumerate() {
            for (x, &p) in row.iter().enumerate() {
                if p == 1 {
                    self.age[(oy + y * 2) * self.width + ox + x] = 1;
                    self.age[(oy + y * 2 + 1) * self.width + ox + x] = 1;
                }
            }
        }
        for _ in 0..self.width * self.height / 12 {
            let i = rng.next_u32() as usize % self.age.len();
            self.age[i] = 1;
        }
    }

    pub fn step(&mut self) {
        let (w, h) = (self.width, self.height);
        let mut next = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut n = 0;
                for (dx, dy) in [(w - 1, h - 1), (0, h - 1), (1, h - 1), (w - 1, 0), (1, 0), (w - 1, 1), (0, 1), (1, 1)] {
                    n += (self.age[((y + dy) % h) * w + (x + dx) % w] > 0) as u8;
                }
                let age = self.age[y * w + x];
                next[y * w + x] = match (age > 0, n) {
                    (true, 2 | 3) => age.saturating_add(1),
                    (false, 3) => 1,
                    _ => 0,
                };
            }
        }
        self.age = next;
    }

    pub fn alive(&self) -> usize {
        self.age.iter().filter(|a| **a > 0).count()
    }
}

/// Young cells glow in `fresh`, settling to `settled` as they age.
pub struct LifeView<'a> {
    pub life: &'a Life,
    pub fresh: Color,
    pub settled: Color,
    pub background: Color,
}

impl Widget for LifeView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = |age: u8| match age {
            0 => self.background,
            1..=2 => self.fresh,
            _ => self.settled,
        };
        let l = self.life;
        let area = area.intersection(buf.area);
        for row in 0..(area.height as usize).min(l.height / 2) {
            for col in 0..(area.width as usize).min(l.width) {
                let top = l.age[row * 2 * l.width + col];
                let bottom = l.age[(row * 2 + 1) * l.width + col];
                buf[(area.x + col as u16, area.y + row as u16)]
                    .set_symbol("▀")
                    .set_fg(color(top))
                    .set_bg(color(bottom));
            }
        }
    }
}

/// HSV (hue in degrees) to a Ratatui RGB color.
pub fn hsv(h: u32, s: u8, v: u8) -> Color {
    let h = h % 360;
    let (s, v) = (s as u32, v as u32);
    let c = v * s / 255;
    let x = c * (60 - (h % 120).abs_diff(60)) / 60;
    let m = v - c;
    let (r, g, b) = match h / 60 {
        0 => (c, x, 0),
        1 => (x, c, 0),
        2 => (0, c, x),
        3 => (0, x, c),
        4 => (x, 0, c),
        _ => (c, 0, x),
    };
    Color::Rgb((r + m) as u8, (g + m) as u8, (b + m) as u8)
}

/// A cheap sine stand-in: phase 0..256 is one turn, output is -128..=128 (two parabolas).
pub fn wave(phase: u32) -> i32 {
    let half = (phase & 127) as i32;
    let v = half * (128 - half) / 32;
    if phase & 128 == 0 { v } else { -v }
}

/// Old-school demoscene plasma: a few interfering waves mapped through a cycling palette.
pub struct Plasma {
    pub t: u32,
}

impl Widget for &Plasma {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let t = self.t;
        let px = |x: u32, y: u32| {
            let (cx, cy) = (x as i32 - 26, y as i32 - 24);
            let v = wave(x * 5 + t)
                + wave(y * 7 + t * 2 / 3)
                + wave((x + y) * 3 + t / 2)
                + wave(((cx * cx + cy * cy) as u32) / 6 + t * 3 / 2);
            // v spans -512..512; one sweep of v is 1.5 trips round the hue wheel.
            let v = (v + 512) as u32;
            hsv(v * 540 / 1024 + t / 3, 200, (140 + wave(v / 2 + t) / 2 + 60).clamp(0, 255) as u8)
        };
        let area = area.intersection(buf.area);
        for row in 0..area.height as u32 {
            for col in 0..area.width as u32 {
                buf[(area.x + col as u16, area.y + row as u16)]
                    .set_symbol("▀")
                    .set_fg(px(col, row * 2))
                    .set_bg(px(col, row * 2 + 1));
            }
        }
    }
}

const RAIN_GLYPHS: &[u8] = b"ratui01<>{}[]|/\\=+*#$%&;:.~^";

/// Digital rain: one falling drop per column leaving a fading trail of glyphs.
pub struct Rain {
    width: usize,
    height: usize,
    /// Head row in 1/16ths, speed in 1/16 rows per step, and trail length, per column.
    drops: Vec<(i32, i32, i32)>,
    glyphs: Vec<u8>,
}

impl Rain {
    pub fn new(width: usize, height: usize, rng: &mut Rng) -> Self {
        let glyphs = (0..width * height).map(|_| RAIN_GLYPHS[rng.next_u32() as usize % RAIN_GLYPHS.len()]).collect();
        let mut rain = Self { width, height, drops: vec![(0, 0, 0); width], glyphs };
        for x in 0..width {
            rain.respawn(x, rng);
            rain.drops[x].0 = (rng.next_u32() % (height as u32 * 32)) as i32;
        }
        rain
    }

    fn respawn(&mut self, x: usize, rng: &mut Rng) {
        let r = rng.next_u32();
        let speed = 6 + (r % 14) as i32;
        let len = 5 + ((r >> 8) % 14) as i32;
        let start = -(((r >> 16) % (self.height as u32 * 16)) as i32);
        self.drops[x] = (start, speed, len);
    }

    pub fn step(&mut self, rng: &mut Rng) {
        for x in 0..self.width {
            let (y, speed, len) = &mut self.drops[x];
            *y += *speed;
            if *y / 16 - *len > self.height as i32 {
                self.respawn(x, rng);
            }
        }
        // A few glyphs flicker each step.
        for _ in 0..self.width / 4 {
            let i = rng.next_u32() as usize % self.glyphs.len();
            self.glyphs[i] = RAIN_GLYPHS[rng.next_u32() as usize % RAIN_GLYPHS.len()];
        }
    }
}

pub struct RainView<'a> {
    pub rain: &'a Rain,
    pub head: Color,
    /// Trail color at full brightness, faded toward black along the trail.
    pub trail: (u8, u8, u8),
}

impl Widget for RainView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let r = self.rain;
        let area = area.intersection(buf.area);
        for col in 0..(area.width as usize).min(r.width) {
            let (y, _, len) = r.drops[col];
            let head = y / 16;
            for row in 0..(area.height as usize).min(r.height) {
                let behind = head - row as i32;
                if !(0..len).contains(&behind) {
                    continue;
                }
                let color = if behind == 0 {
                    self.head
                } else {
                    let k = (len - behind) as u32 * 255 / len as u32;
                    let (cr, cg, cb) = self.trail;
                    Color::Rgb((cr as u32 * k / 255) as u8, (cg as u32 * k / 255) as u8, (cb as u32 * k / 255) as u8)
                };
                let g = r.glyphs[row * r.width + col];
                let mut tmp = [0u8; 4];
                buf[(area.x + col as u16, area.y + row as u16)]
                    .set_symbol((g as char).encode_utf8(&mut tmp))
                    .set_fg(color);
            }
        }
    }
}

/// Stars flying toward the viewer, in a -1..1 square at depth z (1 far, ~0 near).
pub struct Warp {
    stars: Vec<(f32, f32, f32)>,
}

impl Warp {
    pub fn new(count: usize, rng: &mut Rng) -> Self {
        let stars = (0..count)
            .map(|_| {
                let z = Self::unit(rng);
                Self::star(rng, z)
            })
            .collect();
        Self { stars }
    }

    fn unit(rng: &mut Rng) -> f32 {
        (rng.next_u32() % 10_000) as f32 / 10_000.0
    }

    fn star(rng: &mut Rng, z: f32) -> (f32, f32, f32) {
        let x = Self::unit(rng) * 2.0 - 1.0;
        (x, Self::unit(rng) * 2.0 - 1.0, z.max(0.05))
    }

    /// Move every star closer by `speed` (depth units per step); recycle ones that pass.
    pub fn step(&mut self, speed: f32, rng: &mut Rng) {
        for i in 0..self.stars.len() {
            self.stars[i].2 -= speed;
            if self.stars[i].2 <= 0.02 {
                self.stars[i] = Self::star(rng, 1.0);
            }
        }
    }

    /// Streaks from where each star was `trail` depth ago to where it is now, in canvas
    /// coordinates centered on (0, 0) with the given half-extents, nearest last.
    pub fn streaks(&self, trail: f32, half: (f64, f64), mut out: impl FnMut(usize, (f64, f64), (f64, f64))) {
        let project = |x: f32, y: f32, z: f32| ((x / z) as f64 * half.0 * 0.3, (y / z) as f64 * half.1 * 0.3);
        for &(x, y, z) in &self.stars {
            let depth = if z < 0.25 { 2 } else if z < 0.55 { 1 } else { 0 };
            out(depth, project(x, y, z + trail), project(x, y, z));
        }
    }
}
