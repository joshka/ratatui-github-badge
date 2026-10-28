//! The badge UI: a small Ratatui showcase driven by buttons, touch pads and the IMU.
//!
//! Hardware-free so it can be tested on the host with Ratatui's `TestBackend`.

#![no_std]

extern crate alloc;

pub mod config;
pub mod maze;
pub mod mirror;
pub mod qr;
pub mod visuals;
pub mod wordmark;

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
        MascotEyeColor, Paragraph, RatatuiMascot, Row, Sparkline,
        Table, TableState, Tabs, Wrap,
        canvas::{Canvas, Line as CanvasLine, Points},
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

const PAGES: [&str; 7] = ["ratatui", "live", "maze", "widgets", "get", "me", "debug"];
const PAGE_HERO: usize = 0;
const PAGE_LIVE: usize = 1;
const PAGE_MAZE: usize = 2;
const PAGE_WIDGETS: usize = 3;
const PAGE_GET: usize = 4;
const PAGE_CONTACT: usize = 5;
const PAGE_DEBUG: usize = 6;
// Full-screen demo scenes, not in the tab bar.
const PAGE_BILLBOARD: usize = 7;
const PAGE_LIFE: usize = 8;
const PAGE_PLASMA: usize = 9;
const PAGE_RAIN: usize = 10;
const PAGE_WARP: usize = 11;
const SCENE_NAMES: [&str; 5] = ["billboard", "life", "plasma", "rain", "warp"];

const MARQUEE: &str = "ratatui.rs ~ github.com/ratatui/ratatui ~ ";
const FIRE_ROWS: usize = 18;

// Reading a badge you tilt it toward you, so "level" defaults to ~15 degrees off flat:
// sin(15) = 0.26 g toward the bottom of the screen.
const MAZE_NEUTRAL: (f32, f32) = (0.0, 0.26);
const MAZE_COLS: usize = 10;
const MAZE_ROWS: usize = 8;

/// Holding Select this long starts the booth demo. Its script is (page, widgets tab, ms).
const DEMO_HOLD_MS: u32 = 1000;
/// After starting, inputs are ignored this long and until every button is released.
const DEMO_GUARD_MS: u32 = 1500;
const DEMO: [(usize, usize, u32); 11] = [
    (PAGE_BILLBOARD, 0, 12000),
    (PAGE_HERO, 0, 9000),
    (PAGE_LIVE, 0, 7000),
    (PAGE_MAZE, 0, 14000),
    (PAGE_WIDGETS, 0, 5000),
    (PAGE_WIDGETS, 1, 4000),
    (PAGE_WIDGETS, 2, 4000),
    (PAGE_WIDGETS, 3, 9000),
    (PAGE_LIFE, 0, 12000),
    (PAGE_GET, 0, 6000),
    (PAGE_GET, 1, 6000),
];
/// With no input for this long, the badge drifts into ambient mode: just the scenes.
const IDLE_MS: u32 = 30_000;
const AMBIENT: [(usize, usize, u32); 5] = [
    (PAGE_BILLBOARD, 0, 8000),
    (PAGE_PLASMA, 0, 8000),
    (PAGE_LIFE, 0, 10000),
    (PAGE_RAIN, 0, 8000),
    (PAGE_WARP, 0, 8000),
];

const SUBTABS: [&str; 4] = ["stack", "bars", "gauges", "fx"];
const STACK: [[&str; 3]; 7] = [
    ["ratatui", "0.30", "widgets, layout, text"],
    ["mousefood", "0.5", "embedded-graphics backend"],
    ["tachyonfx", "0.25", "cell effects"],
    ["tui-big-text", "0.8", "the big letters"],
    ["embassy-rp", "0.10", "async runtime + HAL"],
    ["RP2350B", "200MHz", "2x Cortex-M33, 520K RAM"],
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

/// Contact QR codes are drawn this many pixels square, with a four-module quiet zone.
const CONTACT_QR_SIDE: u32 = 132;

impl Image {
    /// Width and height in pixels.
    pub fn side(self) -> u32 {
        match self {
            Image::Avatar => config::AVATAR_SIDE,
            Image::Contact(_) => CONTACT_QR_SIDE,
        }
    }

    /// The RGB565 color around the image, filling the rest of its cell-aligned slot.
    pub fn background(self) -> u16 {
        match self {
            Image::Avatar => 0x0863, // BG
            Image::Contact(_) => 0xFFFF,
        }
    }

    /// The RGB565 pixel at (x, y) in a `w` x `h` slot, with the image centered.
    pub fn slot_pixel(self, w: u32, h: u32, x: u32, y: u32) -> u16 {
        let side = self.side();
        let (x, y) = (x.wrapping_sub(w.saturating_sub(side) / 2), y.wrapping_sub(h.saturating_sub(side) / 2));
        if x < side && y < side { self.pixel(x, y) } else { self.background() }
    }

    /// The RGB565 pixel at (x, y), for `x` and `y` below [`Image::side`].
    pub fn pixel(self, x: u32, y: u32) -> u16 {
        match self {
            Image::Avatar => {
                let data = config::AVATAR.unwrap_or(&[]);
                let i = ((y * config::AVATAR_SIDE + x) * 2) as usize;
                data.get(i..i + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
            }
            Image::Contact(i) => {
                let code = &config::CONTACTS[i].qr;
                let modules = code.width as u32 + 8;
                let scale = (CONTACT_QR_SIDE / modules).max(1);
                let offset = (CONTACT_QR_SIDE - modules * scale) / 2 + 4 * scale;
                let module = |p: u32| (p.wrapping_sub(offset) / scale) as usize;
                if code.module(module(x), module(y)) { 0x0000 } else { 0xFFFF }
            }
        }
    }
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
    maze: maze::Maze,
    maze_seed: u32,
    maze_neutral: (f32, f32),
    maze_started_ms: u32,
    maze_last_ms: Option<u32>,
    maze_best_ms: Option<u32>,
    demo: bool,
    /// The running script: `DEMO` (the booth tour) or `AMBIENT` (scenes only).
    script: &'static [(usize, usize, u32)],
    idle_ms: u32,
    demo_step: usize,
    demo_step_ms: u32,
    select_held_ms: u32,
    demo_started_ms: u32,
    demo_guard: bool,
    rng: visuals::Rng,
    fire: visuals::Fire,
    life: visuals::Life,
    rain: visuals::Rain,
    warp: visuals::Warp,
    led_flash_ms: Option<u32>,
    log: Vec<String>,
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
            maze: maze::Maze::new(MAZE_COLS, MAZE_ROWS, 1),
            maze_seed: 1,
            maze_neutral: MAZE_NEUTRAL,
            maze_started_ms: 0,
            maze_last_ms: None,
            maze_best_ms: None,
            demo: false,
            script: &DEMO,
            idle_ms: 0,
            demo_step: 0,
            demo_step_ms: 0,
            select_held_ms: 0,
            demo_started_ms: 0,
            demo_guard: false,
            rng: visuals::Rng(0x1234_5678),
            fire: visuals::Fire::new(53, FIRE_ROWS),
            life: visuals::Life::new(53, 46),
            rain: visuals::Rain::new(53, 24, &mut visuals::Rng(0x9e37_79b9)),
            warp: visuals::Warp::new(90, &mut visuals::Rng(0x85eb_ca6b)),
            led_flash_ms: None,
            log: Vec::new(),
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

    /// Jump straight to a page or demo scene (for previews).
    #[doc(hidden)]
    pub fn goto_page(&mut self, page: usize) {
        self.goto(page);
    }

    pub fn show_debug(&mut self) {
        self.goto(PAGE_DEBUG);
    }

    pub fn demo(&self) -> bool {
        self.demo
    }

    /// Whether the running demo is the idle scenes rather than the booth tour.
    pub fn ambient(&self) -> bool {
        self.demo && core::ptr::eq(self.script, &AMBIENT[..])
    }

    /// Events since the last call, for the USB log.
    pub fn take_log(&mut self) -> Vec<String> {
        core::mem::take(&mut self.log)
    }

    fn flash_leds(&mut self) {
        self.led_flash_ms = Some(self.now_ms);
    }

    /// Case LED brightness (linear 0-255), front-left to front-right.
    pub fn leds(&self) -> [u8; 4] {
        let t = self.now_ms;
        let tri = |period: u32, phase: u32| {
            let p = (t + phase) % period;
            let half = period / 2;
            (if p < half { p } else { period - p }) * 255 / half
        };
        let mut out = if self.demo {
            // A chase that sweeps across the four LEDs, over a slow breathe.
            let lead = (t / 140) % 6;
            let lead = if lead < 4 { lead } else { 6 - lead };
            core::array::from_fn(|i| if i as u32 == lead { 255 } else { 20 + tri(2000, 0) / 6 })
        } else {
            core::array::from_fn(|i| (12 + tri(3200, i as u32 * 400) / 5) as u8 as u32)
        };
        if self.page == PAGE_BILLBOARD {
            out = core::array::from_fn(|i| 80 + tri(600, i as u32 * 150) * 2 / 3);
        }
        if let Some(start) = self.led_flash_ms {
            let age = t.wrapping_sub(start);
            if age < 500 {
                let boost = 255 * (500 - age) / 500;
                out = out.map(|v| v.max(boost));
            }
        }
        out.map(|v| v.min(255) as u8)
    }

    /// The first input during the demo only ends it.
    fn wake(&mut self) -> bool {
        self.idle_ms = 0;
        if !self.demo {
            return false;
        }
        // The hold that started the demo (and any bounce on release) mustn't end it.
        if self.demo_guard {
            return true;
        }
        self.demo = false;
        self.log.push("demo off".into());
        self.goto(PAGE_HERO);
        true
    }

    fn accent(&self) -> Color {
        ACCENTS[self.accent]
    }

    /// A physical button was pressed; directions follow the screen when flipped.
    pub fn key(&mut self, key: Key) {
        if self.wake() {
            return;
        }
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
        if page == PAGE_MAZE && self.page != PAGE_MAZE {
            self.maze_started_ms = self.now_ms;
        }
        if page == PAGE_LIFE {
            self.life.seed(&mut self.rng);
        }
        self.page = page;
        let name = PAGES.get(page).copied().unwrap_or_else(|| SCENE_NAMES[page - PAGE_BILLBOARD]);
        self.log.push(format!("page {name}"));
        self.clear = true;
        let effect = fx::coalesce((350, Interpolation::QuadOut)).with_area(content_area());
        self.fx.add_unique_effect(0, effect);
    }

    fn adjust(&mut self, forward: bool) {
        let step = |v: usize, n: usize| if forward { (v + 1) % n } else { (v + n - 1) % n };
        match self.page {
            PAGE_HERO => self.mood = step(self.mood, config::MOODS.len()),
            PAGE_WIDGETS => {
                self.sub = step(self.sub, SUBTABS.len());
                let area = widgets_body();
                let direction = if forward { Motion::RightToLeft } else { Motion::LeftToRight };
                let effect = fx::slide_in(direction, 8, 0, BG, (300, Interpolation::QuadOut));
                self.fx.add_unique_effect(1, effect.with_area(area));
            }
            PAGE_LIVE => self.accent = step(self.accent, ACCENTS.len()),
            PAGE_MAZE if forward => self.maze_neutral = MAZE_NEUTRAL,
            PAGE_MAZE => self.maze_neutral = self.screen_gravity(),
            PAGE_GET => self.qr = step(self.qr, 2),
            PAGE_CONTACT => self.contact = step(self.contact, config::CONTACTS.len()),
            _ => {}
        }
    }

    fn select(&mut self) {
        self.flash_leds();
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
            PAGE_MAZE => self.new_maze(),
            _ => {
                let effect = fx::sweep_in(Motion::LeftToRight, 12, 0, BG, (450, Interpolation::QuadOut));
                self.fx.add_unique_effect(1, effect.with_area(content_area()));
            }
        }
    }

    fn new_maze(&mut self) {
        self.maze_seed = self.maze_seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) | 1;
        self.maze = maze::Maze::new(MAZE_COLS, MAZE_ROWS, self.maze_seed);
        self.maze_started_ms = self.now_ms;
        let effect = fx::coalesce((500, Interpolation::QuadOut)).with_area(content_area());
        self.fx.add_unique_effect(1, effect);
    }

    /// Gravity in screen axes (+x right, +y down), following the rotation.
    fn screen_gravity(&self) -> (f32, f32) {
        let [ax, ay, _] = self.sensors.accel;
        let sign = if self.flipped { -1.0 } else { 1.0 };
        (ax * sign, ay * sign)
    }

    fn update_maze(&mut self, dt_ms: u32) {
        if self.demo {
            self.maze.autopilot(dt_ms);
        } else {
            let (gx, gy) = self.screen_gravity();
            let tilt = (gx - self.maze_neutral.0, gy - self.maze_neutral.1);
            self.maze.roll(tilt, dt_ms);
        }
        if self.maze.solved() {
            let time = self.now_ms.wrapping_sub(self.maze_started_ms);
            self.flash_leds();
            self.log.push(format!("maze solved in {}.{}s{}", time / 1000, time % 1000 / 100, if self.demo { " (autopilot)" } else { "" }));
            if !self.demo {
                self.maze_last_ms = Some(time);
                self.maze_best_ms = Some(self.maze_best_ms.map_or(time, |b| b.min(time)));
            }
            self.new_maze();
            let boom = fx::explode(10.0, 3.0, (700, Interpolation::QuadOut));
            let effect = fx::sequence(&[boom, fx::coalesce((500, Interpolation::QuadOut))]);
            self.fx.add_unique_effect(1, effect.with_area(content_area()));
        }
    }

    /// Start the booth demo (hold Select, or the USB `demo` command). Takes over from
    /// ambient mode.
    pub fn start_demo(&mut self) {
        if self.demo && !self.ambient() {
            return;
        }
        self.run_script(&DEMO, "demo on");
        self.demo_guard = true;
    }

    /// Start the idle scenes (after `IDLE_MS` without input, or the USB `ambient` command).
    pub fn start_ambient(&mut self) {
        if !self.demo {
            self.run_script(&AMBIENT, "ambient on");
        }
    }

    fn run_script(&mut self, script: &'static [(usize, usize, u32)], log: &str) {
        self.demo = true;
        self.script = script;
        self.demo_started_ms = self.now_ms;
        self.log.push(log.into());
        self.demo_step = script.len() - 1;
        self.demo_step_ms = u32::MAX;
        self.flash_leds();
    }

    fn update_demo(&mut self, dt_ms: u32) {
        // Physical Select is bit 4 of `buttons`, in `Key::ALL` order.
        if self.sensors.buttons & 0b10000 != 0 && self.sensors.buttons & 0b00100 == 0 {
            self.select_held_ms = self.select_held_ms.saturating_add(dt_ms);
            if self.select_held_ms >= DEMO_HOLD_MS && (!self.demo || self.ambient()) {
                self.select_held_ms = 0;
                self.start_demo();
            }
        } else {
            self.select_held_ms = 0;
        }
        if !self.demo {
            // The maze is played by tilting, so it never counts as idle.
            let busy = self.sensors.buttons != 0 || self.page == PAGE_MAZE;
            self.idle_ms = if busy { 0 } else { self.idle_ms.saturating_add(dt_ms) };
            if self.idle_ms >= IDLE_MS {
                self.start_ambient();
            }
            return;
        }
        if self.demo_guard
            && self.sensors.buttons == 0
            && self.now_ms.wrapping_sub(self.demo_started_ms) >= DEMO_GUARD_MS
        {
            self.demo_guard = false;
        }
        self.demo_step_ms = self.demo_step_ms.saturating_add(dt_ms);
        let (_, _, duration) = self.script[self.demo_step];
        if self.demo_step_ms >= duration {
            self.demo_step = (self.demo_step + 1) % self.script.len();
            self.demo_step_ms = 0;
            let (page, sub, _) = self.script[self.demo_step];
            if page == PAGE_GET {
                self.qr = sub;
            }
            if page != self.page {
                self.goto(page);
            }
            if page == PAGE_WIDGETS && sub != self.sub {
                self.sub = sub;
                let effect = fx::slide_in(Motion::RightToLeft, 8, 0, BG, (300, Interpolation::QuadOut));
                self.fx.add_unique_effect(1, effect.with_area(widgets_body()));
            }
            self.accent = (self.accent + 1) % ACCENTS.len();
        }
        // Keep the effects page busy and the mascot lively.
        let tick = |period| self.demo_step_ms / period != self.demo_step_ms.saturating_sub(dt_ms) / period;
        if self.page == PAGE_WIDGETS && self.sub == 3 && tick(1800) {
            let effect = effect_named(self.effect).with_area(widgets_body());
            self.fx.add_unique_effect(1, effect);
            self.effect = (self.effect + 1) % EFFECTS.len();
        }
        if self.page == PAGE_HERO && tick(3000) {
            self.select();
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
        if new_touches != 0 && !self.wake() && self.page == PAGE_HERO {
            self.eye_red_until = self.now_ms + 300;
        }

        if sensors.imu_ok {
            self.update_orientation(sensors.accel[FLIP_AXIS] * FLIP_SIGN);
        }
        if self.page == PAGE_MAZE {
            self.update_maze(dt_ms);
        }
        if self.page == PAGE_BILLBOARD {
            self.fire.step(&mut self.rng);
        }
        if self.page == PAGE_RAIN && self.now_ms / 40 != (self.now_ms - dt_ms) / 40 {
            self.rain.step(&mut self.rng);
        }
        if self.page == PAGE_WARP {
            // Lean the badge to steer; the stars speed up and slow down on a slow swell.
            let swell = visuals::wave(self.now_ms / 40) as f32 / 128.0;
            self.warp.step((0.012 + swell * 0.006) * dt_ms as f32 / 33.0, &mut self.rng);
        }
        if self.page == PAGE_LIFE && self.now_ms / 120 != (self.now_ms - dt_ms) / 120 {
            self.life.step();
            if self.life.alive() < 40 {
                self.life.seed(&mut self.rng);
            }
        }
        if self.page == PAGE_HERO && self.now_ms / 6000 != (self.now_ms - dt_ms) / 6000 {
            let shimmer = fx::sweep_in(Motion::LeftToRight, 12, 0, BG, (900, Interpolation::QuadOut));
            self.fx.add_unique_effect(3, shimmer.with_area(wordmark_area(19)));
        }
        self.update_demo(dt_ms);
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
            self.log.push(format!("flipped {wants_flip}"));
            self.flash_leds();
            if wants_flip {
                self.wake();
            }
            self.clear = true;
            let effect = fx::sweep_in(Motion::UpToDown, 10, 0, BG, (400, Interpolation::QuadOut));
            self.fx.add_unique_effect(0, effect);
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        self.images.clear();
        let area = f.area();
        f.render_widget(Block::new().style(Style::new().bg(BG).fg(TEXT)), area);
        if self.page >= PAGE_BILLBOARD {
            match self.page {
                PAGE_BILLBOARD => self.draw_billboard(f),
                PAGE_LIFE => self.draw_life(f),
                PAGE_PLASMA => self.draw_plasma(f),
                PAGE_RAIN => self.draw_rain(f),
                _ => self.draw_warp(f),
            }
            self.draw_scene_banner(f);
            let dt = Duration::from_millis(self.dt_ms);
            self.fx.process_effects(dt, f.buffer_mut(), area);
            return;
        }
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
            PAGE_MAZE => self.draw_maze(f, body),
            PAGE_WIDGETS => self.draw_widgets(f, body),
            PAGE_GET => self.draw_get(f, body),
            PAGE_CONTACT => self.draw_contact(f, body),
            _ => self.draw_debug(f, body),
        }
        let hints = if self.demo { "demo · press any button" } else { match self.page {
            PAGE_HERO => "◄► page ▲▼ status hold● demo",
            PAGE_LIVE => "tilt me · touch the pads",
            PAGE_WIDGETS => "▲▼ tab  ● play effect",
            PAGE_MAZE => "▲ level here ▼ reset ● new",
            PAGE_GET => "▲▼ switch link",
            PAGE_CONTACT => "▲▼ choose · scan",
            _ => "hold ◄+● for bootloader",
        } };
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
        f.render_widget(
            Paragraph::new(config::HANDLE).centered().style(Style::new().fg(self.accent()).bold()),
            Rect::new(panel.x, 2, panel.width, 1),
        );
        if config::AVATAR.is_some() {
            let avatar = Rect::new(37, 4, 15, 9);
            mark_image(f, avatar);
            self.images.push((Image::Avatar, avatar));
        }
        f.render_widget(
            Paragraph::new(vec![
                Line::from(config::NAME).bold(),
                Line::from(config::ROLE).fg(self.accent()),
                Line::from(""),
                Line::from(config::MOODS[self.mood]).fg(TEXT).italic(),
            ])
            .centered()
            .wrap(Wrap { trim: true }),
            Rect::new(panel.x, if config::AVATAR.is_some() { 13 } else { 5 }, panel.width, 5),
        );

        f.render_widget(self.wordmark(), wordmark_area(19));
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

    fn wordmark(&self) -> wordmark::Wordmark {
        wordmark::Wordmark { letters: Color::Rgb(255, 255, 255), shadow: wordmark::SHADOW }
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
                    ctx.draw(&Points { coords: &ring(0.0, 0.0, 1.0, 72), color: DIM });
                    ctx.draw(&CanvasLine::new(-1.0, 0.0, 1.0, 0.0, DIM));
                    ctx.draw(&CanvasLine::new(0.0, -1.0, 0.0, 1.0, DIM));
                    ctx.draw(&Points { coords: &ring(bx, by, 0.18, 20), color: accent });
                    ctx.draw(&Points { coords: &ring(bx, by, 0.08, 10), color: accent });
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
                .data(self.light_history)
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

    fn draw_billboard(&mut self, f: &mut Frame) {
        let area = f.area();
        let hue = self.now_ms / 20;
        f.render_widget(
            visuals::BigWordmark { letters: Color::Rgb(255, 255, 255), shadow: wordmark::SHADOW },
            Rect::new(0, 1, area.width, visuals::BigWordmark::HEIGHT),
        );
        let offset = (self.now_ms / 45) as usize;
        visuals::marquee(f.buffer_mut(), Rect::new(0, 7, area.width, 8), MARQUEE, offset, |x| {
            visuals::hsv(hue + x as u32 * 4, 160, 255)
        });
        let fire_rows = (FIRE_ROWS / 2) as u16;
        f.render_widget(&self.fire, Rect::new(0, area.height - fire_rows, area.width, fire_rows));
    }

    fn draw_life(&mut self, f: &mut Frame) {
        let area = f.area();
        let accent = self.accent();
        f.render_widget(
            visuals::LifeView { life: &self.life, fresh: Color::Rgb(255, 255, 255), settled: accent, background: BG },
            Rect::new(0, 0, area.width, 23),
        );
    }

    /// Every scene's bottom row: who's wearing the badge, and what it's built with.
    fn draw_scene_banner(&mut self, f: &mut Frame) {
        let area = f.area();
        let row = Rect::new(0, area.height - 1, area.width, 1);
        let who = Line::from(vec![
            Span::styled(format!(" {} ", config::NAME), Style::new().fg(TEXT).bold()),
            Span::styled(config::HANDLE, Style::new().fg(self.accent())),
        ]);
        let ratatui = Line::from(vec![
            Span::styled("built with ", Style::new().fg(DIM)),
            Span::styled("Ratatui ", Style::new().fg(self.accent()).bold()),
        ]);
        f.render_widget(ratatui::widgets::Clear, row);
        f.render_widget(Block::new().style(Style::new().bg(BG)), row);
        f.render_widget(Paragraph::new(who), row);
        f.render_widget(Paragraph::new(ratatui).right_aligned(), row);
    }

    fn draw_plasma(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(&visuals::Plasma { t: self.now_ms / 12 }, area);
        let big = BigText::builder()
            .pixel_size(PixelSize::Quadrant)
            .style(Style::new().fg(Color::Rgb(255, 255, 255)))
            .lines(vec!["ratatui".into()])
            .centered()
            .build();
        f.render_widget(big, Rect::new(0, area.height / 2 - 2, area.width, 4));
    }

    fn draw_rain(&mut self, f: &mut Frame) {
        let area = f.area();
        let trail = match self.accent() {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => (80, 240, 200),
        };
        f.render_widget(visuals::RainView { rain: &self.rain, head: Color::Rgb(255, 255, 255), trail }, area);
        let msg = " ratatui.rs ";
        let x = (area.width - msg.len() as u16) / 2;
        let style = Style::new().fg(BG).bg(self.accent()).bold();
        f.render_widget(Paragraph::new(msg).style(style), Rect::new(x, area.height / 2, msg.len() as u16, 1));
    }

    fn draw_warp(&mut self, f: &mut Frame) {
        let area = f.area();
        // Canvas units: one per braille dot (2x4 per cell), centered and nudged by the tilt.
        let half = (area.width as f64, area.height as f64 * 2.0);
        let (tx, ty) = (self.tilt.0 as f64 * half.0 * 0.4, self.tilt.1 as f64 * half.1 * 0.4);
        let mut layers: [Vec<(f64, f64, f64, f64)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        self.warp.streaks(0.06, half, |depth, (x1, y1), (x2, y2)| {
            layers[depth].push((x1 - tx, y1 - ty, x2 - tx, y2 - ty));
        });
        let colors = [DIM, Color::Rgb(170, 180, 220), Color::Rgb(255, 255, 255)];
        let accent = self.accent();
        f.render_widget(
            Canvas::default()
                .marker(Marker::Braille)
                .background_color(BG)
                .x_bounds([-half.0, half.0])
                .y_bounds([-half.1, half.1])
                .paint(|ctx| {
                    for (layer, color) in layers.iter().zip(colors) {
                        for &(x1, y1, x2, y2) in layer {
                            ctx.draw(&CanvasLine { x1, y1, x2, y2, color });
                        }
                        ctx.layer();
                    }
                    ctx.print(-4.0, 0.0, Span::styled("ratatui", Style::new().fg(accent).bold()));
                }),
            area,
        );
    }

    fn draw_maze(&mut self, f: &mut Frame, body: Rect) {
        let accent = self.accent();
        let (w, h) = self.maze.size();
        let area = Rect::new((body.width - w) / 2, body.y, w, h);
        f.render_widget(
            maze::MazeView {
                maze: &self.maze,
                wall: Color::Rgb(70, 80, 130),
                floor: BG,
                trail: Color::Rgb(36, 38, 60),
                ball: accent,
            },
            area,
        );
        let secs = |ms: u32| format!("{}.{}s", ms / 1000, (ms % 1000) / 100);
        let elapsed = self.now_ms.wrapping_sub(self.maze_started_ms);
        let mut spans = vec![Span::styled(format!(" time {} ", secs(elapsed)), Style::new().fg(BG).bg(accent))];
        if let Some(last) = self.maze_last_ms {
            spans.push(Span::styled(format!("  last {}", secs(last)), TEXT));
        }
        if let Some(best) = self.maze_best_ms {
            spans.push(Span::styled(format!("  best {}", secs(best)), Color::Rgb(255, 210, 100)));
        }
        if self.demo {
            spans = vec![Span::styled(" autopilot: breadth-first search ", Style::new().fg(BG).bg(accent))];
        }
        f.render_widget(Paragraph::new(Line::from(spans)).centered(), Rect::new(0, area.bottom(), body.width, 1));
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
        f.render_widget(self.wordmark(), wordmark_area(body.y + 1));
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
                Line::from(config::CONTACTS[self.contact].label).fg(accent).bold(),
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
            Line::from(format!(
                "frame {} ms = draw {} + flush {}",
                s.frame_ms,
                s.frame_ms.saturating_sub(s.flush_ms),
                s.flush_ms
            )),
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

/// Points on a circle by repeated rotation. Ratatui's `Circle` evaluates f64 sin/cos at all
/// 360 degrees, which this FPU-less-for-f64 chip emulates in software (~25 ms per circle).
fn ring(x: f64, y: f64, radius: f64, points: usize) -> Vec<(f64, f64)> {
    // cos/sin of 2*pi/points via a short Taylor series; points >= 8 keeps the error tiny.
    let a = core::f64::consts::TAU / points as f64;
    let (a2, a3) = (a * a, a * a * a);
    let (c, s) = (1.0 - a2 / 2.0 + a2 * a2 / 24.0, a - a3 / 6.0 + a3 * a2 / 120.0);
    let (mut dx, mut dy) = (radius, 0.0);
    (0..points)
        .map(|_| {
            let p = (x + dx, y + dy);
            (dx, dy) = (dx * c - dy * s, dx * s + dy * c);
            p
        })
        .collect()
}

fn wrap100(v: f64) -> f64 {
    let r = v % 100.0;
    if r < 0.0 { r + 100.0 } else { r }
}

fn round(v: f32) -> i16 {
    if v < 0.0 { (v - 0.5) as i16 } else { (v + 0.5) as i16 }
}

fn wordmark_area(y: u16) -> Rect {
    let (w, h) = wordmark::Wordmark::SIZE;
    Rect::new((53 - w) / 2, y, w, h)
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
