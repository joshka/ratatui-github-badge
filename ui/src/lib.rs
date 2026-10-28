//! The badge UI: a small Ratatui showcase driven by buttons, touch pads and the IMU.
//!
//! Hardware-free so it can be tested on the host with Ratatui's `TestBackend`.

#![no_std]

extern crate alloc;

pub mod qr;

use alloc::{format, string::String, vec, vec::Vec};

use ratatui::{
    Frame,
    buffer::CellDiffOption,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Bar, BarChart, BarGroup, Block, Chart, Dataset, Gauge, GraphType, LineGauge,
        MascotEyeColor, Paragraph, RatatuiLogo, RatatuiLogoSize, RatatuiMascot, Row, Sparkline,
        Table, TableState, Tabs, Wrap,
        canvas::{Canvas, Circle, Line as CanvasLine, Points},
    },
};
use tachyonfx::{Duration, Effect, EffectManager, Interpolation, Motion, fx};
use tui_big_text::{BigText, PixelSize};

pub const BG: Color = Color::Rgb(10, 12, 24);
const DIM: Color = Color::Rgb(90, 96, 120);
const TEXT: Color = Color::Rgb(220, 224, 236);
const ACCENTS: [Color; 4] = [
    Color::Rgb(80, 240, 200),
    Color::Rgb(255, 120, 200),
    Color::Rgb(150, 150, 255),
    Color::Rgb(255, 210, 100),
];

const PAGES: [&str; 6] = ["ratatui", "live", "widgets", "get it", "contact", "debug"];
const PAGE_HERO: usize = 0;
const PAGE_LIVE: usize = 1;
const PAGE_WIDGETS: usize = 2;
const PAGE_GET: usize = 3;
const PAGE_CONTACT: usize = 4;

const MOODS: [&str; 4] = [
    "Ask me about Ratatui",
    "Come see the Ratatui booth",
    "Let's build a TUI",
    "Open to chat",
];
pub const CONTACT_LABELS: [&str; 3] = ["Save contact", "github.com/ferris", "ratatui.rs"];
const SUBTABS: [&str; 4] = ["stack", "bars", "gauges", "fx"];
const STACK: [[&str; 3]; 7] = [
    ["ratatui", "0.30", "widgets, layout, text"],
    ["mousefood", "0.5", "embedded-graphics backend"],
    ["tachyonfx", "0.25", "cell effects"],
    ["tui-big-text", "0.8", "the big letters"],
    ["embassy-rp", "0.10", "async runtime + HAL"],
    ["RP2350B", "150MHz", "2x Cortex-M33, 520K RAM"],
    ["ST7789", "320x240", "8-bit parallel LCD"],
];
const EFFECTS: [&str; 6] = ["coalesce", "dissolve", "sweep", "slide", "hue shift", "explode"];
const KEY_GLYPHS: [&str; 5] = ["▲", "▼", "◄", "►", "●"];

/// Pages with an image slot; the firmware draws the pixels after Mousefood flushes cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Image {
    Avatar,
    Contact(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
}

impl Key {
    const ALL: [Key; 5] = [Key::Up, Key::Down, Key::Left, Key::Right, Key::Select];

    fn rotated(self) -> Self {
        match self {
            Key::Up => Key::Down,
            Key::Down => Key::Up,
            Key::Left => Key::Right,
            Key::Right => Key::Left,
            Key::Select => Key::Select,
        }
    }
}

/// One frame's worth of hardware readings.
#[derive(Clone, Copy, Default, Debug)]
pub struct Sensors {
    /// Acceleration in g, IMU axes.
    pub accel: [f32; 3],
    pub imu_ok: bool,
    pub battery_v: f32,
    /// Ambient light, 0..1.
    pub light: f32,
    /// CAP1208 touch bits, CAP1 = bit 0.
    pub touch: u8,
    pub touch_ok: bool,
    /// Physical buttons held, in `Key::ALL` order.
    pub buttons: u8,
    pub heap_used: usize,
    pub heap_free: usize,
    pub frame_ms: u32,
    pub flush_ms: u32,
}

// Gravity along the screen's vertical axis decides orientation. Hanging upright from a
// lanyard reads about +1 g on Y; flipped up to read it, Y goes negative.
const FLIP_AXIS: usize = 1;
const FLIP_SIGN: f32 = -1.0;
const FLIP_THRESHOLD_G: f32 = 0.45;
const FLIP_HOLD_MS: u32 = 300;

const HISTORY: usize = 96;
const STARS: usize = 70;

pub struct App {
    pub page: usize,
    sub: usize,
    mood: usize,
    contact: usize,
    qr: usize,
    accent: usize,
    effect: usize,
    now_ms: u32,
    dt_ms: u32,
    sensors: Sensors,
    tilt: (f32, f32),
    accel_history: [[f32; 3]; HISTORY],
    light_history: [u64; 48],
    touch_counts: [u64; 8],
    key_counts: [u64; 5],
    prev_touch: u8,
    flipped: bool,
    flip_since: Option<u32>,
    eye_red_until: u32,
    stack: TableState,
    fx: EffectManager<u8>,
    clear: bool,
    pub images: Vec<(Image, Rect)>,
    pub report: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let mut app = Self {
            page: PAGE_HERO,
            sub: 0,
            mood: 0,
            contact: 0,
            qr: 0,
            accent: 0,
            effect: 0,
            now_ms: 0,
            dt_ms: 0,
            sensors: Sensors::default(),
            tilt: (0.0, 0.0),
            accel_history: [[0.0; 3]; HISTORY],
            light_history: [0; 48],
            touch_counts: [0; 8],
            key_counts: [0; 5],
            prev_touch: 0,
            flipped: false,
            flip_since: None,
            eye_red_until: 0,
            stack: TableState::default().with_selected(Some(0)),
            fx: EffectManager::default(),
            clear: true,
            images: Vec::new(),
            report: None,
        };
        app.fx.add_unique_effect(0, fx::coalesce((900, Interpolation::QuadOut)));
        app
    }

    pub fn flipped(&self) -> bool {
        self.flipped
    }

    /// True once after anything that needs a full redraw (page change, rotation).
    pub fn take_clear(&mut self) -> bool {
        core::mem::take(&mut self.clear)
    }

    fn accent(&self) -> Color {
        ACCENTS[self.accent]
    }

    /// A physical button was pressed; directions follow the screen when flipped.
    pub fn key(&mut self, key: Key) {
        let key = if self.flipped { key.rotated() } else { key };
        if let Some(i) = Key::ALL.iter().position(|k| *k == key) {
            self.key_counts[i] += 1;
        }
        match key {
            Key::Left => self.goto((self.page + PAGES.len() - 1) % PAGES.len()),
            Key::Right => self.goto((self.page + 1) % PAGES.len()),
            Key::Up | Key::Down => self.adjust(key == Key::Down),
            Key::Select => self.select(),
        }
    }

    fn goto(&mut self, page: usize) {
        self.page = page;
        self.clear = true;
        let effect = fx::coalesce((350, Interpolation::QuadOut)).with_area(content_area());
        self.fx.add_unique_effect(0, effect);
    }

    fn adjust(&mut self, forward: bool) {
        let step = |v: usize, n: usize| if forward { (v + 1) % n } else { (v + n - 1) % n };
        match self.page {
            PAGE_HERO => self.mood = step(self.mood, MOODS.len()),
            PAGE_WIDGETS => {
                self.sub = step(self.sub, SUBTABS.len());
                let area = widgets_body();
                let direction = if forward { Motion::RightToLeft } else { Motion::LeftToRight };
                let effect = fx::slide_in(direction, 8, 0, BG, (300, Interpolation::QuadOut));
                self.fx.add_unique_effect(1, effect.with_area(area));
            }
            PAGE_LIVE => self.accent = step(self.accent, ACCENTS.len()),
            PAGE_GET => self.qr = step(self.qr, 2),
            PAGE_CONTACT => self.contact = step(self.contact, CONTACT_LABELS.len()),
            _ => {}
        }
    }

    fn select(&mut self) {
        match self.page {
            PAGE_HERO => {
                self.eye_red_until = self.now_ms + 700;
                self.accent = (self.accent + 1) % ACCENTS.len();
                let shift = fx::hsl_shift_fg([120.0, 0.0, 0.0], (350, Interpolation::SineInOut));
                self.fx.add_unique_effect(2, fx::ping_pong(shift).with_area(mascot_area(0, 0)));
            }
            PAGE_WIDGETS if self.sub == 3 => {
                let effect = effect_named(self.effect).with_area(widgets_body());
                self.fx.add_unique_effect(1, effect);
                self.effect = (self.effect + 1) % EFFECTS.len();
            }
            PAGE_WIDGETS => {
                self.sub = 3;
            }
            _ => {
                let effect = fx::sweep_in(Motion::LeftToRight, 12, 0, BG, (450, Interpolation::QuadOut));
                self.fx.add_unique_effect(1, effect.with_area(content_area()));
            }
        }
    }

    /// Advance time and take new sensor readings.
    pub fn update(&mut self, dt_ms: u32, sensors: Sensors) {
        self.dt_ms = dt_ms;
        self.now_ms = self.now_ms.wrapping_add(dt_ms);
        self.sensors = sensors;

        // X is side-to-side; Z (lean toward/away) is ~0 while hanging, unlike Y which
        // carries gravity. Mirror both when the screen is rotated.
        let [ax, _, az] = sensors.accel;
        let sign = if self.flipped { -1.0 } else { 1.0 };
        let k = 0.15;
        self.tilt.0 += ((ax * sign).clamp(-1.0, 1.0) - self.tilt.0) * k;
        self.tilt.1 += ((az * sign).clamp(-1.0, 1.0) - self.tilt.1) * k;
        self.accel_history.rotate_left(1);
        self.accel_history[HISTORY - 1] = sensors.accel;
        self.light_history.rotate_left(1);
        self.light_history[47] = (sensors.light * 100.0) as u64;

        let new_touches = sensors.touch & !self.prev_touch;
        self.prev_touch = sensors.touch;
        for (i, count) in self.touch_counts.iter_mut().enumerate() {
            if new_touches & (1 << i) != 0 {
                *count += 1;
            }
        }
        if new_touches != 0 && self.page == PAGE_HERO {
            self.eye_red_until = self.now_ms + 300;
        }

        if sensors.imu_ok {
            self.update_orientation(sensors.accel[FLIP_AXIS] * FLIP_SIGN);
        }
        if self.now_ms / 1500 != (self.now_ms - dt_ms) / 1500 {
            self.stack.select_next();
            if self.stack.selected() >= Some(STACK.len()) {
                self.stack.select(Some(0));
            }
        }
    }

    fn update_orientation(&mut self, g: f32) {
        let wants_flip = if self.flipped { g > -FLIP_THRESHOLD_G } else { g > FLIP_THRESHOLD_G };
        if wants_flip == self.flipped {
            self.flip_since = None;
            return;
        }
        let since = *self.flip_since.get_or_insert(self.now_ms);
        if self.now_ms.wrapping_sub(since) >= FLIP_HOLD_MS {
            self.flipped = wants_flip;
            self.flip_since = None;
            self.clear = true;
            let effect = fx::sweep_in(Motion::UpToDown, 10, 0, BG, (400, Interpolation::QuadOut));
            self.fx.add_unique_effect(0, effect);
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        self.images.clear();
        let area = f.area();
        f.render_widget(Block::new().style(Style::new().bg(BG).fg(TEXT)), area);
        let [top, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        f.render_widget(
            Tabs::new(PAGES)
                .select(self.page)
                .style(DIM)
                .highlight_style(Style::new().fg(BG).bg(self.accent()).bold())
                .divider("│"),
            top,
        );
        match self.page {
            PAGE_HERO => self.draw_hero(f, body),
            PAGE_LIVE => self.draw_live(f, body),
            PAGE_WIDGETS => self.draw_widgets(f, body),
            PAGE_GET => self.draw_get(f, body),
            PAGE_CONTACT => self.draw_contact(f, body),
            _ => self.draw_debug(f, body),
        }
        let hints = match self.page {
            PAGE_HERO => "◄► page  ▲▼ status  ● poke",
            PAGE_LIVE => "tilt me · touch the pads",
            PAGE_WIDGETS => "▲▼ tab  ● play effect",
            PAGE_GET => "▲▼ switch link",
            PAGE_CONTACT => "▲▼ choose · scan",
            _ => "hold ◄+● for bootloader",
        };
        let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(11)]).areas(footer);
        f.render_widget(Paragraph::new(hints).style(DIM), left);
        f.render_widget(Paragraph::new("ratatui.rs").style(self.accent()).right_aligned(), right);

        let dt = Duration::from_millis(self.dt_ms);
        self.fx.process_effects(dt, f.buffer_mut(), area);
    }

    fn draw_hero(&mut self, f: &mut Frame, body: Rect) {
        // Starfield: three depths drifting left, shifted against the tilt for parallax.
        let t = self.now_ms as f64 / 1000.0;
        let (tx, ty) = (self.tilt.0 as f64, self.tilt.1 as f64);
        let mut layers: [Vec<(f64, f64)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let mut seed = 0x2545_f491u32;
        for _ in 0..STARS {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let depth = (seed % 3) as usize;
            let speed = [2.0, 5.0, 11.0][depth];
            let x = ((seed >> 8) % 1000) as f64 / 10.0;
            let y = ((seed >> 18) % 1000) as f64 / 10.0;
            let px = wrap100(x - t * speed + tx * speed * 0.8);
            let py = wrap100(y + ty * speed * 0.8);
            layers[depth].push((px, py));
        }
        let colors = [Color::Rgb(60, 70, 110), Color::Rgb(120, 130, 180), Color::Rgb(220, 230, 255)];
        f.render_widget(
            Canvas::default()
                .marker(Marker::Braille)
                .x_bounds([0.0, 100.0])
                .y_bounds([0.0, 100.0])
                .paint(|ctx| {
                    for (layer, color) in layers.iter().zip(colors) {
                        ctx.draw(&Points { coords: layer, color });
                    }
                }),
            body,
        );

        let dx = round(self.tilt.0 * 2.0).clamp(-1, 2);
        let dy = round(self.tilt.1).clamp(-1, 0);
        let blink = self.now_ms % 4200 < 140 || self.now_ms < self.eye_red_until;
        let eye = if blink { MascotEyeColor::Red } else { MascotEyeColor::Default };
        f.render_widget(RatatuiMascot::new().set_eye(eye), mascot_area(dx, dy));

        let panel = Rect::new(36, body.y, 17, 17);
        let logo = Rect::new(37, 1, 15, 2);
        f.render_widget(RatatuiLogo::new(RatatuiLogoSize::Tiny), logo);
        f.buffer_mut().set_style(logo, self.accent());
        let avatar = Rect::new(37, 4, 15, 9);
        mark_image(f, avatar);
        self.images.push((Image::Avatar, avatar));
        f.render_widget(
            Paragraph::new(vec![
                Line::from("Ferris the Crab").bold(),
                Line::from("maintainer").fg(self.accent()),
                Line::from(""),
                Line::from(MOODS[self.mood]).fg(TEXT).italic(),
            ])
            .centered()
            .wrap(Wrap { trim: true }),
            Rect::new(panel.x, 13, panel.width, 5),
        );

        f.render_widget(
            BigText::builder()
                .pixel_size(PixelSize::Quadrant)
                .style(Style::new().fg(self.accent()))
                .lines(vec!["@ferris".into()])
                .alignment(Alignment::Center)
                .build(),
            Rect::new(0, 18, body.width, 4),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("ratatui.rs", Style::new().fg(self.accent()).bold()),
                Span::styled("  ·  ", DIM),
                Span::styled("github.com/ratatui/ratatui", TEXT),
            ]))
            .centered(),
            Rect::new(0, 22, body.width, 1),
        );
    }

    fn draw_live(&mut self, f: &mut Frame, body: Rect) {
        let s = self.sensors;
        let accent = self.accent();
        let [top, gauges, inputs] =
            Layout::vertical([Constraint::Length(11), Constraint::Length(3), Constraint::Fill(1)]).areas(body);
        let [chart_area, level_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(19)]).areas(top);

        let series: [Vec<(f64, f64)>; 3] = core::array::from_fn(|axis| {
            self.accel_history.iter().enumerate().map(|(i, a)| (i as f64, a[axis] as f64)).collect()
        });
        let names = ["x", "y", "z"];
        let colors = [Color::Rgb(255, 100, 120), Color::Rgb(100, 230, 140), Color::Rgb(110, 160, 255)];
        let datasets = (0..3)
            .map(|i| {
                Dataset::default()
                    .name(names[i])
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(colors[i])
                    .data(&series[i])
            })
            .collect();
        f.render_widget(
            Chart::new(datasets)
                .block(Block::bordered().title(" accelerometer (g) ").border_style(accent))
                .x_axis(Axis::default().bounds([0.0, (HISTORY - 1) as f64]))
                .y_axis(Axis::default().bounds([-1.5, 1.5])),
            chart_area,
        );

        let (bx, by) = (self.tilt.0 as f64, -self.tilt.1 as f64);
        f.render_widget(
            Canvas::default()
                .block(Block::bordered().title(" tilt ").border_style(accent))
                .marker(Marker::Braille)
                .x_bounds([-1.2, 1.2])
                .y_bounds([-1.2, 1.2])
                .paint(move |ctx| {
                    ctx.draw(&Circle { x: 0.0, y: 0.0, radius: 1.0, color: DIM });
                    ctx.draw(&CanvasLine::new(-1.0, 0.0, 1.0, 0.0, DIM));
                    ctx.draw(&CanvasLine::new(0.0, -1.0, 0.0, 1.0, DIM));
                    ctx.draw(&Circle { x: bx, y: by, radius: 0.18, color: accent });
                    ctx.draw(&Circle { x: bx, y: by, radius: 0.08, color: accent });
                }),
            level_area,
        );

        let [battery, light] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(gauges);
        let charge = ((s.battery_v - 3.3) / (4.2 - 3.3)).clamp(0.0, 1.0);
        f.render_widget(
            Gauge::default()
                .block(Block::bordered().title(" battery ").border_style(accent))
                .gauge_style(Style::new().fg(Color::Rgb(100, 230, 140)).bg(Color::Rgb(30, 34, 50)))
                .ratio(charge as f64)
                .label(format!("{:.2} V", s.battery_v)),
            battery,
        );
        f.render_widget(
            Sparkline::default()
                .block(Block::bordered().title(" light ").border_style(accent))
                .data(&self.light_history)
                .max(100)
                .style(Color::Rgb(255, 210, 100)),
            light,
        );

        let pad = |on: bool, label: String| {
            let style = if on { Style::new().fg(BG).bg(accent).bold() } else { Style::new().fg(DIM) };
            Span::styled(format!(" {label} "), style)
        };
        let mut touch = vec![Span::styled("touch ", DIM)];
        for i in 0..8 {
            touch.push(pad(s.touch & (1 << i) != 0, format!("{}", i + 1)));
        }
        let mut keys = vec![Span::styled("keys  ", DIM)];
        for (i, glyph) in KEY_GLYPHS.iter().enumerate() {
            keys.push(pad(s.buttons & (1 << i) != 0, String::from(*glyph)));
            keys.push(Span::raw(" "));
        }
        let status = format!(
            "imu {}  touch {}  {} fps",
            if s.imu_ok { "ok" } else { "--" },
            if s.touch_ok { "ok" } else { "--" },
            1000 / s.frame_ms.max(1),
        );
        f.render_widget(
            Paragraph::new(vec![Line::from(touch), Line::from(""), Line::from(keys), Line::from(""), Line::from(status).fg(DIM)])
                .block(Block::bordered().title(" inputs ").border_style(accent)),
            inputs,
        );
    }

    fn draw_widgets(&mut self, f: &mut Frame, body: Rect) {
        let accent = self.accent();
        let [tabs, content] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(body);
        f.render_widget(
            Tabs::new(SUBTABS)
                .select(self.sub)
                .style(DIM)
                .highlight_style(Style::new().fg(accent).bold().add_modifier(Modifier::UNDERLINED)),
            tabs,
        );
        let block = Block::bordered().border_style(accent);
        match self.sub {
            0 => {
                let rows = STACK.iter().map(|r| Row::new(r.iter().copied()));
                f.render_stateful_widget(
                    Table::new(rows, [Constraint::Length(12), Constraint::Length(7), Constraint::Fill(1)])
                        .header(Row::new(["crate", "ver", "role"]).style(Style::new().fg(accent).bold()))
                        .row_highlight_style(Style::new().fg(BG).bg(accent))
                        .highlight_symbol("▶")
                        .block(block.title(" what this badge runs ")),
                    content,
                    &mut self.stack,
                );
            }
            1 => {
                let labels = ["1", "2", "3", "4", "5", "6", "7", "8"];
                let mut bars: Vec<Bar> = labels
                    .iter()
                    .zip(self.touch_counts)
                    .map(|(l, v)| Bar::default().label(Line::from(*l)).value(v).style(accent))
                    .collect();
                bars.extend(
                    KEY_GLYPHS
                        .iter()
                        .zip(self.key_counts)
                        .map(|(l, v)| Bar::default().label(Line::from(*l)).value(v).style(Color::Rgb(255, 210, 100))),
                );
                f.render_widget(
                    BarChart::default()
                        .block(block.title(" presses: touch pads + keys "))
                        .data(BarGroup::default().bars(&bars))
                        .bar_width(3)
                        .bar_gap(0),
                    content,
                );
            }
            2 => {
                let s = self.sensors;
                let heap = s.heap_used as f64 / (s.heap_used + s.heap_free).max(1) as f64;
                let inner = block.inner(content);
                f.render_widget(block.title(" gauges, all live "), content);
                let rows: [Rect; 6] = Layout::vertical([Constraint::Length(2); 6]).areas(inner);
                let line = |label: &str, ratio: f64, color: Color| {
                    LineGauge::default()
                        .label(String::from(label))
                        .ratio(ratio.clamp(0.0, 1.0))
                        .filled_style(color)
                        .unfilled_style(DIM)
                };
                let frame = (s.frame_ms as f64 / 100.0).min(1.0);
                f.render_widget(line("heap   ", heap, accent), rows[0]);
                f.render_widget(line("battery", ((s.battery_v - 3.3) / 0.9) as f64, Color::Rgb(100, 230, 140)), rows[1]);
                f.render_widget(line("light  ", s.light as f64, Color::Rgb(255, 210, 100)), rows[2]);
                f.render_widget(line("tilt x ", (self.tilt.0 as f64 + 1.0) / 2.0, Color::Rgb(255, 100, 120)), rows[3]);
                f.render_widget(line("tilt y ", (self.tilt.1 as f64 + 1.0) / 2.0, Color::Rgb(110, 160, 255)), rows[4]);
                f.render_widget(line("frame  ", frame, Color::Rgb(150, 150, 255)), rows[5]);
            }
            _ => {
                let inner = block.inner(content);
                f.render_widget(block.title(" tachyonfx "), content);
                let [big, list] = Layout::vertical([Constraint::Length(8), Constraint::Fill(1)]).areas(inner);
                f.render_widget(
                    BigText::builder()
                        .pixel_size(PixelSize::HalfHeight)
                        .style(Style::new().fg(accent))
                        .lines(vec!["fx!".into()])
                        .alignment(Alignment::Center)
                        .build(),
                    big,
                );
                let names: Vec<Line> = EFFECTS
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        if i == self.effect {
                            Line::from(format!("● next: {name}")).fg(accent).bold()
                        } else {
                            Line::from(format!("  {name}")).fg(DIM)
                        }
                    })
                    .collect();
                f.render_widget(Paragraph::new(names).centered(), list);
            }
        }
    }

    fn draw_get(&mut self, f: &mut Frame, body: Rect) {
        let accent = self.accent();
        f.render_widget(
            BigText::builder()
                .pixel_size(PixelSize::Quadrant)
                .style(Style::new().fg(accent))
                .lines(vec!["ratatui".into()])
                .alignment(Alignment::Center)
                .build(),
            Rect::new(0, body.y, body.width, 4),
        );
        let code = if self.qr == 0 { &qr::QR_RATATUI_RS } else { &qr::QR_GITHUB };
        let (w, h) = code.size();
        let qr_area = Rect::new(1, body.y + 4 + (18u16.saturating_sub(h)) / 2, w, h);
        f.render_widget(code, qr_area);
        let text_x = qr_area.right() + 1;
        let text_area = Rect::new(text_x, body.y + 5, body.width.saturating_sub(text_x), 17);
        let lines = if self.qr == 0 {
            vec![
                Line::from("ratatui.rs").fg(accent).bold(),
                Line::from(""),
                Line::from("Docs, tutorials,"),
                Line::from("examples and"),
                Line::from("a showcase of apps"),
                Line::from(""),
                Line::from("cargo add ratatui").fg(Color::Rgb(255, 210, 100)),
                Line::from(""),
                Line::from("▼ GitHub").fg(DIM),
            ]
        } else {
            vec![
                Line::from("github.com/").fg(accent).bold(),
                Line::from("ratatui/ratatui").fg(accent).bold(),
                Line::from(""),
                Line::from("Star it ♥"),
                Line::from("Good first issues"),
                Line::from("welcome"),
                Line::from(""),
                Line::from("▲ ratatui.rs").fg(DIM),
            ]
        };
        f.render_widget(Paragraph::new(lines), text_area);
    }

    fn draw_contact(&mut self, f: &mut Frame, body: Rect) {
        let accent = self.accent();
        let slot = Rect::new((body.width - 22) / 2, body.y + 2, 22, 14);
        mark_image(f, slot);
        self.images.push((Image::Contact(self.contact), slot));
        f.render_widget(
            Paragraph::new(vec![
                Line::from(CONTACT_LABELS[self.contact]).fg(accent).bold(),
                Line::from("scan with your phone camera").fg(DIM),
            ])
            .centered(),
            Rect::new(0, slot.bottom() + 1, body.width, 2),
        );
    }

    fn draw_debug(&mut self, f: &mut Frame, body: Rect) {
        let s = self.sensors;
        let [ax, ay, az] = s.accel;
        let mut lines = vec![
            Line::from(format!("frame {} ms (flush {} ms)", s.frame_ms, s.flush_ms)),
            Line::from(format!("heap {}K used / {}K free", s.heap_used / 1024, s.heap_free / 1024)),
            Line::from(format!("accel {ax:+.2} {ay:+.2} {az:+.2} g  imu {}", s.imu_ok)),
            Line::from(format!("touch {:08b}  ok {}", s.touch, s.touch_ok)),
            Line::from(format!("battery {:.2} V  light {:.2}", s.battery_v, s.light)),
            Line::from(format!("flipped {}  (axis {FLIP_AXIS})", self.flipped)),
            Line::from(""),
            Line::from("last failure").fg(self.accent()),
        ];
        match &self.report {
            Some(r) => lines.push(Line::from(r.clone())),
            None => lines.push(Line::from("none this boot").fg(DIM)),
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" debug ").border_style(self.accent())),
            body,
        );
    }
}

fn wrap100(v: f64) -> f64 {
    let r = v % 100.0;
    if r < 0.0 { r + 100.0 } else { r }
}

fn round(v: f32) -> i16 {
    if v < 0.0 { (v - 0.5) as i16 } else { (v + 0.5) as i16 }
}

fn content_area() -> Rect {
    Rect::new(0, 1, 53, 22)
}

fn widgets_body() -> Rect {
    Rect::new(0, 2, 53, 21)
}

fn mascot_area(dx: i16, dy: i16) -> Rect {
    Rect::new((1 + dx) as u16, (2 + dy) as u16, 32, 16)
}

fn effect_named(index: usize) -> Effect {
    match index {
        0 => fx::coalesce((800, Interpolation::QuadOut)),
        1 => fx::sequence(&[fx::dissolve((500, Interpolation::QuadIn)), fx::coalesce((500, Interpolation::QuadOut))]),
        2 => fx::sweep_in(Motion::LeftToRight, 15, 4, BG, (800, Interpolation::QuadOut)),
        3 => fx::slide_in(Motion::UpToDown, 10, 3, BG, (700, Interpolation::BounceOut)),
        4 => fx::ping_pong(fx::hsl_shift_fg([180.0, 0.0, 0.0], (600, Interpolation::SineInOut))),
        _ => fx::explode(12.0, 3.0, (900, Interpolation::QuadOut)),
    }
}

/// Reserve cells for a bitmap; Mousefood skips them and the firmware draws the pixels.
fn mark_image(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_diff_option(CellDiffOption::Skip);
        }
    }
}

/// The screen is 53x24 cells of 6x10 px (Mousefood's default font).
pub const CELL_PX: (u16, u16) = (6, 10);
