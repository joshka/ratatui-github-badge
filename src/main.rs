//! Ratatui on the GitHub Universe 2026 badge (RP2350B, 320x240 ST7789).
//!
//! The UI lives in the hardware-free `badge-ui` crate; this file wires up the display,
//! buttons and sensors. Hold Left + Select for a second to reboot into BOOTSEL.

#![no_std]
#![no_main]

mod lcd;
mod recovery;
mod sensors;

extern crate alloc;

use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    mem::MaybeUninit,
};

use badge_ui::{App, CELL_PX, Image, Key};
use embassy_executor::Spawner;
use embassy_rp::{
    adc::{self, Adc},
    gpio::{Input, Level, Output, Pull},
    i2c::I2c,
};
use embassy_time::{Duration, Instant, Ticker};
use embedded_alloc::LlffHeap as Heap;
use embedded_graphics::{
    image::{Image as EgImage, ImageRawBE},
    pixelcolor::Rgb565,
    prelude::*,
};
use mousefood::prelude::*;
use ratatui::{Terminal, layout::Rect};

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 3] = [
    embassy_rp::binary_info::rp_program_name!(c"ratatui-badge"),
    embassy_rp::binary_info::rp_program_description!(c"Ratatui showcase on the Universe badge"),
    embassy_rp::binary_info::rp_cargo_version!(),
];

#[global_allocator]
static HEAP: Heap = Heap::empty();

static mut FRAMEBUFFER: [u16; lcd::WIDTH * lcd::HEIGHT] = [0; lcd::WIDTH * lcd::HEIGHT];

static AVATAR: &[u8] = include_bytes!("../assets/avatar.rgb565");
static CONTACT_CODES: [&[u8]; 3] = [
    include_bytes!("../assets/contact.rgb565"),
    include_bytes!("../assets/github.rgb565"),
    include_bytes!("../assets/linkedin.rgb565"),
];

/// What the flush callback needs from the latest frame.
#[derive(Default)]
struct FlushState {
    flipped: bool,
    images: Vec<(Image, Rect)>,
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(_spawner: Spawner) {
    {
        const HEAP_SIZE: usize = 160 * 1024;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(&raw mut HEAP_MEM as usize, HEAP_SIZE) }
    }

    let p = embassy_rp::init(Default::default());
    recovery::init();
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(5));

    let data = [
        Output::new(p.PIN_32, Level::Low),
        Output::new(p.PIN_33, Level::Low),
        Output::new(p.PIN_34, Level::Low),
        Output::new(p.PIN_35, Level::Low),
        Output::new(p.PIN_36, Level::Low),
        Output::new(p.PIN_37, Level::Low),
        Output::new(p.PIN_38, Level::Low),
        Output::new(p.PIN_39, Level::Low),
    ];
    // The executor creates this display once; no other code accesses the framebuffer.
    let framebuffer = &raw mut FRAMEBUFFER;
    // Also powers the switched 3V3 rail that the IMU and touch controller sit on.
    let mut display = lcd::Lcd::new(
        Output::new(p.PIN_27, Level::High), // CS
        Output::new(p.PIN_28, Level::High), // DC
        Output::new(p.PIN_30, Level::High), // WR
        Output::new(p.PIN_31, Level::High), // RD
        data,
        Output::new(p.PIN_26, Level::Low), // backlight
        Output::new(p.PIN_41, Level::Low), // POWER_EN
        unsafe { &mut *framebuffer },
    );

    let mut sensors = sensors::Sensors::new(
        I2c::new_blocking(p.I2C1, p.PIN_19, p.PIN_18, sensors::i2c_config()),
        Adc::new_blocking(p.ADC, adc::Config::default()),
        adc::Channel::new_pin(p.PIN_40, Pull::None),
        adc::Channel::new_pin(p.PIN_43, Pull::None),
    );

    // In `Key::ALL` order: Up, Down, Left (A), Right (B), Select (C).
    let buttons = [
        Input::new(p.PIN_11, Pull::Up),
        Input::new(p.PIN_6, Pull::Up),
        Input::new(p.PIN_7, Pull::Up),
        Input::new(p.PIN_9, Pull::Up),
        Input::new(p.PIN_10, Pull::Up),
    ];
    const KEYS: [Key; 5] = [Key::Up, Key::Down, Key::Left, Key::Right, Key::Select];

    let shared = Rc::new(RefCell::new(FlushState::default()));
    let flush_ms = Rc::new(Cell::new(0u32));
    let config = EmbeddedBackendConfig {
        flush_callback: alloc::boxed::Box::new({
            let shared = shared.clone();
            let flush_ms = flush_ms.clone();
            move |d: &mut lcd::Lcd| {
                let start = Instant::now();
                let state = shared.borrow();
                for (image, rect) in &state.images {
                    draw_image(d, *image, *rect);
                }
                d.flush(state.flipped);
                flush_ms.set(start.elapsed().as_millis() as u32);
            }
        }),
        ..Default::default()
    };
    let backend = EmbeddedBackend::new(&mut display, config);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new();
    if let Some(report) = recovery::report() {
        app.report = Some(report.as_str().into());
        app.page = 5;
    }
    let mut previous = 0u8;
    let mut combo_since: Option<Instant> = None;
    let mut last = Instant::now();
    let mut frame_ms = 0u32;
    let mut ticker = Ticker::every(Duration::from_millis(33));

    loop {
        let start = Instant::now();
        let held = buttons
            .iter()
            .enumerate()
            .fold(0u8, |bits, (i, b)| bits | (u8::from(b.is_low()) << i));
        let pressed = held & !previous;
        previous = held;

        // Physical Left + Select, regardless of screen rotation.
        if held & 0b10100 == 0b10100 {
            let since = *combo_since.get_or_insert(start);
            if since.elapsed() > Duration::from_secs(1) {
                embassy_rp::rom_data::reboot(0x0002 | 0x0100, 10, 0, 0);
            }
        } else {
            combo_since = None;
            for (i, key) in KEYS.iter().enumerate() {
                if pressed & (1 << i) != 0 {
                    app.key(*key);
                }
            }
        }

        let readings = badge_ui::Sensors {
            accel: sensors.accel().unwrap_or_default(),
            imu_ok: sensors.imu_ok,
            battery_v: sensors.battery_v(),
            light: sensors.light(),
            touch: sensors.touch(),
            touch_ok: sensors.touch_ok,
            buttons: held,
            heap_used: HEAP.used(),
            heap_free: HEAP.free(),
            frame_ms,
            flush_ms: flush_ms.get(),
        };
        let dt = start.duration_since(last).as_millis() as u32;
        last = start;
        app.update(dt, readings);
        if app.take_clear() {
            terminal.clear().unwrap();
        }
        terminal
            .draw(|f| {
                app.draw(f);
                let mut state = shared.borrow_mut();
                state.flipped = app.flipped();
                state.images.clone_from(&app.images);
            })
            .unwrap();

        frame_ms = start.elapsed().as_millis() as u32;
        watchdog.feed(Duration::from_secs(5));
        ticker.next().await;
    }
}

fn draw_image(display: &mut lcd::Lcd, image: Image, rect: Rect) {
    let (data, side) = match image {
        Image::Avatar => (AVATAR, 88),
        Image::Contact(i) => (CONTACT_CODES[i], 132),
    };
    let (cw, ch) = (CELL_PX.0 as i32, CELL_PX.1 as i32);
    let (w, h) = (rect.width as i32 * cw, rect.height as i32 * ch);
    let origin = Point::new(
        rect.x as i32 * cw + (w - side) / 2,
        rect.y as i32 * ch + (h - side) / 2,
    );
    let raw = ImageRawBE::<Rgb565>::new(data, side as u32);
    EgImage::new(&raw, origin).draw(display).unwrap();
}
