//! Ratatui on the GitHub Universe 2026 badge (RP2350B, 320x240 ST7789).
//!
//! The UI lives in the hardware-free `badge-ui` crate; this file wires up the display,
//! buttons and sensors. Hold Left + Select for a second to reboot into BOOTSEL.

#![no_std]
#![no_main]

mod lcd;
mod leds;
mod recovery;
mod sensors;
mod usb;

extern crate alloc;

use alloc::{format, rc::Rc, string::String, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    mem::MaybeUninit,
};

use badge_ui::{App, CELL_PX, Image, Key};
use embassy_executor::Spawner;
use embassy_rp::{
    adc::{self, Adc},
    executor::InterruptExecutor,
    interrupt::{self, InterruptExt, Priority},
    gpio::{Input, Level, Output, Pull},
    i2c::I2c,
    pwm::Pwm,
};
use embassy_time::{Duration, Instant, Ticker};
use embedded_alloc::LlffHeap as Heap;
use embedded_graphics::{
    pixelcolor::{Rgb565, raw::RawU16},
    prelude::*,
    primitives::Rectangle,
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

static USB_EXECUTOR: InterruptExecutor = InterruptExecutor::new();

#[embassy_rp::interrupt]
unsafe fn SWI_IRQ_1() {
    unsafe { USB_EXECUTOR.on_interrupt() }
}

static mut FRAMEBUFFER: [u16; lcd::WIDTH * lcd::HEIGHT] = [0; lcd::WIDTH * lcd::HEIGHT];

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
        const HEAP_SIZE: usize = 184 * 1024;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(&raw mut HEAP_MEM as usize, HEAP_SIZE) }
    }

    // Pimoroni's firmware runs this chip at 200 MHz; embassy picks the matching core voltage.
    let mut config = embassy_rp::config::Config::default();
    config.clocks = embassy_rp::clocks::ClockConfig::system_freq(200_000_000).unwrap();
    let p = embassy_rp::init(config);
    // The default 500-entry layout cache outgrows the heap; our pages need a few dozen.
    ratatui::layout::Layout::init_cache(core::num::NonZeroUsize::new(64).unwrap());
    recovery::init();
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(5));
    interrupt::SWI_IRQ_1.set_priority(Priority::P2);
    usb::start(USB_EXECUTOR.start(interrupt::SWI_IRQ_1), p.USB);
    let mut leds = leds::Leds::new(
        Pwm::new_output_ab(p.PWM_SLICE0, p.PIN_0, p.PIN_1, leds::Leds::config()),
        Pwm::new_output_ab(p.PWM_SLICE1, p.PIN_2, p.PIN_3, leds::Leds::config()),
    );

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
    let flush_us = Rc::new(Cell::new(0u64));
    let config = EmbeddedBackendConfig {
        flush_callback: alloc::boxed::Box::new({
            let shared = shared.clone();
            let flush_ms = flush_ms.clone();
            let flush_us = flush_us.clone();
            move |d: &mut lcd::Lcd| {
                let start = Instant::now();
                let state = shared.borrow();
                for (image, rect) in &state.images {
                    draw_image(d, *image, *rect);
                }
                d.flush(state.flipped);
                flush_ms.set(start.elapsed().as_millis() as u32);
                flush_us.set(start.elapsed().as_micros());
            }
        }),
        ..Default::default()
    };
    let backend = EmbeddedBackend::new(&mut display, config);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new();
    if let Some(report) = recovery::report() {
        app.report = Some(report.as_str().into());
        app.show_debug();
    }
    let mut previous = 0u8;
    let mut combo_since: Option<Instant> = None;
    let mut last = Instant::now();
    let mut frame_ms = 0u32;
    // Most pages now draw in ~20 ms; cap at ~60 fps and let slower pages run back to back.
    let mut ticker = Ticker::every(Duration::from_millis(16));
    let mut mirror = false;
    let mut snapshot = false;
    let mut frame_count = 0u32;
    // Only grows when mirroring or taking a snapshot.
    let mut text = String::new();
    // Rolling sums for the USB perf log: (render, backend, flush) in microseconds.
    let mut perf = [0u64; 3];
    let mut perf_frames = 0u64;
    let mut perf_since = Instant::now();

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
                usb::reboot(0x0002);
            }
        } else {
            combo_since = None;
            for (i, key) in KEYS.iter().enumerate() {
                if pressed & (1 << i) != 0 {
                    app.key(*key);
                }
            }
        }

        if usb::reboot_requested() {
            usb::reboot(0x0002);
        }
        while let Ok(command) = usb::COMMANDS.try_receive() {
            match command {
                usb::Command::Key(key) => app.key(key),
                usb::Command::Mirror(on) => {
                    mirror = on;
                    if !mirror {
                        text = String::new();
                    }
                }
                usb::Command::Screen => snapshot = true,
                usb::Command::Bootsel => usb::reboot(0x0002),
                usb::Command::Demo => app.start_demo(),
                usb::Command::Ambient => app.start_ambient(),
                usb::Command::Help => {
                    usb::write("commands: mirror [on|off] | screen | demo | ambient | key up|down|left|right|select | bootsel\r\n");
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
        let mut render_us = 0;
        let draw_start = Instant::now();
        let completed = terminal
            .draw(|f| {
                let render_start = Instant::now();
                app.draw(f);
                render_us = render_start.elapsed().as_micros();
                let mut state = shared.borrow_mut();
                state.flipped = app.flipped();
                state.images.clone_from(&app.images);
            })
            .unwrap();
        let draw_us = draw_start.elapsed().as_micros();
        perf[0] += render_us;
        perf[1] += draw_us.saturating_sub(render_us + flush_us.get());
        perf[2] += flush_us.get();
        perf_frames += 1;
        frame_count = frame_count.wrapping_add(1);
        // Mirror every 3rd frame, skipping frames the host hasn't caught up with.
        if mirror && frame_count.is_multiple_of(3) {
            badge_ui::mirror::ansi(completed.buffer, &mut text);
            usb::write(&text);
        }
        if core::mem::take(&mut snapshot) {
            badge_ui::mirror::text(completed.buffer, &mut text);
            usb::write(&text);
        }
        for line in app.take_log() {
            usb::write(&format!("[{:>7}] {line}\r\n", Instant::now().as_millis()));
        }
        leds.set(app.leds());
        if perf_since.elapsed() > Duration::from_secs(2) {
            let [r, b, fl] = perf.map(|v| v / perf_frames / 100);
            let line = format!(
                "perf page {} render {}.{} backend {}.{} flush {}.{} ms heap {}K\r\n",
                app.page, r / 10, r % 10, b / 10, b % 10, fl / 10, fl % 10, HEAP.used() / 1024
            );
            usb::write(&line);
            (perf, perf_frames, perf_since) = ([0; 3], 0, Instant::now());
        }

        frame_ms = start.elapsed().as_millis() as u32;
        watchdog.feed(Duration::from_secs(5));
        ticker.next().await;
    }
}

fn draw_image(display: &mut lcd::Lcd, image: Image, rect: Rect) {
    let (w, h) = (rect.width as u32 * CELL_PX.0 as u32, rect.height as u32 * CELL_PX.1 as u32);
    let pixels = (0..w * h).map(|i| Rgb565::from(RawU16::new(image.slot_pixel(w, h, i % w, i / w))));
    let origin = Point::new(rect.x as i32 * CELL_PX.0 as i32, rect.y as i32 * CELL_PX.1 as i32);
    display.fill_contiguous(&Rectangle::new(origin, Size::new(w, h)), pixels).unwrap();
}
