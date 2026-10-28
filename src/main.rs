//! Ratatui on the GitHub Universe 2026 badge (RP2350B, 320x240 ST7789).
//!
//! The UI lives in the hardware-free `badge-ui` crate; this file wires up the display,
//! buttons, sensors and host links (USB, Bluetooth LE, the Wi-Fi relay). Hold Left + Select for
//! a second to reboot into BOOTSEL, or RESET to sleep.

#![no_std]
#![no_main]

mod lcd;
mod leds;
mod link;
mod ota;
mod radio;
mod recovery;
mod sensors;
mod sleep;
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

/// A secure Arm executable, flagged "try before you buy": after an update the ROM boots it on
/// trial, and `ota.rs` confirms it once it has run for a while (embassy's default lacks the
/// flag). The version, from build.rs, makes the ROM prefer the newest image of the two.
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: embassy_rp::block::Block<3> = {
    use embassy_rp::block::*;
    const fn parse(s: &str) -> u32 {
        let (bytes, mut i, mut n) = (s.as_bytes(), 0, 0);
        while i < bytes.len() {
            n = n * 10 + (bytes[i] - b'0') as u32;
            i += 1;
        }
        n
    }
    let major = parse(env!("BADGE_VERSION_MAJOR"));
    let minor = parse(env!("BADGE_VERSION_MINOR"));
    Block::new([
        item_image_type_exe(Security::Secure, Architecture::Arm) | (IMAGE_TYPE_TBYB as u32) << 16,
        item_generic_2bs(0, 2, ITEM_1BS_VERSION),
        major << 16 | minor,
    ])
};

/// The largest mirror frame to build. Its buffer stays allocated while mirroring, so it is kept
/// well under the USB outbox's 32 KB: the busiest pages skip frames rather than starve the heap.
const MIRROR_MAX: usize = 24 * 1024;

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
        // Pages use around 100 KB, but switching pages needs 15 KB blocks (a 53x24 cell buffer);
        // at 168 KB the heap fragmented badly enough to fail one.
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
    // Safe mode: after a stall or panic, skip optional subsystems for one boot, so a crash in
    // one of them can't become a reset loop that needs BOOT + RESET to flash a fix.
    let safe_mode = recovery::init();
    let mut leds = leds::Leds::new(
        Pwm::new_output_ab(p.PWM_SLICE0, p.PIN_0, p.PIN_1, leds::Leds::config()),
        Pwm::new_output_ab(p.PWM_SLICE1, p.PIN_2, p.PIN_3, leds::Leds::config()),
    );
    sleep::check(p.PIN_14, &mut leds);
    let mut ota = ota::Ota::new(p.FLASH);
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(5));
    interrupt::SWI_IRQ_1.set_priority(Priority::P2);
    usb::start(USB_EXECUTOR.start(interrupt::SWI_IRQ_1), p.USB);

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
    // After the display, which drives POWER_EN: the radio sits on that switched rail too.
    radio::start(
        p.CORE1,
        safe_mode,
        radio::Pins {
            pio: p.PIO0,
            dma_tx: p.DMA_CH0,
            dma_rx: p.DMA_CH1,
            trng: p.TRNG,
            power: p.PIN_23,
            data: p.PIN_24,
            cs: p.PIN_25,
            clock: p.PIN_29,
        },
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
    // Say what booted once a host has had a few seconds to connect.
    let boot = Instant::now();
    let mut ota_announce = true;
    let mut combo_since: Option<Instant> = None;
    let mut last = Instant::now();
    let mut frame_ms = 0u32;
    // Most pages now draw in ~20 ms; cap at ~60 fps and let slower pages run back to back.
    let mut ticker = Ticker::every(Duration::from_millis(16));
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
        while let Ok((source, line)) = link::OTA.try_receive() {
            ota.command(source, &line);
        }
        ota.poll(&mut watchdog);
        if ota_announce && start.duration_since(boot) > Duration::from_secs(3) {
            ota_announce = false;
            link::write(&format!("{}\r\n", ota.describe()));
        }
        while let Ok(command) = link::COMMANDS.try_receive() {
            match command {
                link::Command::Key(key) => app.key(key),
                link::Command::Screen => snapshot = true,
                link::Command::Bootsel => usb::reboot(0x0002),
                link::Command::Demo => app.start_demo(),
                link::Command::Ambient => app.start_ambient(),
                link::Command::Help => link::write(link::HELP),
            }
        }
        let mirror = link::mirroring();
        if !mirror && !text.is_empty() {
            text = String::new();
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
            // Reserved once at full size, so it never doubles.
            text.reserve(MIRROR_MAX.saturating_sub(text.capacity()));
            if badge_ui::mirror::ansi(completed.buffer, &mut text, MIRROR_MAX) {
                link::write_frame(&text);
            }
        }
        if core::mem::take(&mut snapshot) {
            badge_ui::mirror::text(completed.buffer, &mut text);
            link::write(&text);
        }
        for line in app.take_log() {
            link::write(&format!("[{:>7}] {line}\r\n", Instant::now().as_millis()));
        }
        leds.set(app.leds());
        if perf_since.elapsed() > Duration::from_secs(2) {
            let [r, b, fl] = perf.map(|v| v / perf_frames / 100);
            let line = format!(
                "perf page {} render {}.{} backend {}.{} flush {}.{} ms heap {}K\r\n",
                app.page, r / 10, r % 10, b / 10, b % 10, fl / 10, fl % 10, HEAP.used() / 1024
            );
            link::write(&line);
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
