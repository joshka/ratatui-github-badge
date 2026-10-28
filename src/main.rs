//! Ratatui on the GitHub Universe 2026 badge (RP2350B, 320x240 ST7789).
//!
//! Left/Right browse pages; Enter performs the displayed action and Up/Down adjust it.
//! Hold Left + Enter for a second to reboot into BOOTSEL.

#![no_std]
#![no_main]

mod badge;
mod lcd;
mod recovery;
mod timer;
mod wifi;

use alloc::rc::Rc;
use core::{cell::Cell, mem::MaybeUninit};
use embassy_executor::Spawner;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_time::{Duration, Instant, Ticker};
use embedded_alloc::LlffHeap as Heap;
use mousefood::prelude::*;
use ratatui::Terminal;

#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 3] = [
    embassy_rp::binary_info::rp_program_name!(c"ratatui-badge"),
    embassy_rp::binary_info::rp_program_description!(c"Ratatui + mousefood on the Universe badge"),
    embassy_rp::binary_info::rp_cargo_version!(),
];

#[global_allocator]
static HEAP: Heap = Heap::empty();

fn heap_usage() -> (usize, usize) {
    (HEAP.used(), HEAP.free())
}

static mut FRAMEBUFFER: [u16; lcd::WIDTH * lcd::HEIGHT] = [0; lcd::WIDTH * lcd::HEIGHT];

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    {
        const HEAP_SIZE: usize = 160 * 1024;
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(&raw mut HEAP_MEM as usize, HEAP_SIZE) }
    }

    let p = embassy_rp::init(Default::default());
    let recovered = recovery::init();
    let mut watchdog = embassy_rp::watchdog::Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(5));
    spawner.spawn(
        wifi::run(
            spawner, p.PIO0, p.DMA_CH0, p.PIN_23, p.PIN_25, p.PIN_24, p.PIN_29,
        )
        .unwrap(),
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

    let btn_up = Input::new(p.PIN_11, Pull::Up);
    let btn_down = Input::new(p.PIN_6, Pull::Up);
    let btn_a = Input::new(p.PIN_7, Pull::Up);
    let btn_b = Input::new(p.PIN_9, Pull::Up);
    let btn_c = Input::new(p.PIN_10, Pull::Up);

    let state = Rc::new(Cell::new(badge::Badge::default()));
    let drawing_state = state.clone();
    let config = EmbeddedBackendConfig {
        flush_callback: alloc::boxed::Box::new(move |d: &mut lcd::Lcd| {
            drawing_state.get().overlay(d);
            d.flush();
        }),
        ..Default::default()
    };
    let backend = EmbeddedBackend::new(&mut display, config);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut badge = badge::Badge::default();
    if recovered {
        badge.page = 6;
    }
    let mut previous = [false; 5];
    let mut combo_since: Option<Instant> = None;
    let mut last_frame = Instant::now();
    let mut ticker = Ticker::every(Duration::from_millis(50));

    loop {
        let now = Instant::now();
        badge.advance(now.duration_since(last_frame).as_millis());
        last_frame = now;
        let keys = [
            btn_up.is_low(),
            btn_down.is_low(),
            btn_a.is_low(),
            btn_b.is_low(),
            btn_c.is_low(),
        ];
        let pressed = core::array::from_fn::<_, 5, _>(|i| keys[i] && !previous[i]);
        previous = keys;
        if keys[2] && keys[4] {
            let since = *combo_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(1) {
                embassy_rp::rom_data::reboot(0x0002 | 0x0100, 10, 0, 0);
            }
        } else {
            combo_since = None;
            let old_page = badge.page;
            if pressed[0] {
                badge.adjust(false);
            }
            if pressed[1] {
                badge.adjust(true);
            }
            if pressed[2] {
                badge.navigate(false);
            }
            if pressed[3] {
                badge.navigate(true);
            }
            if pressed[4] {
                badge.activate();
            }
            if old_page != badge.page {
                terminal.clear().unwrap();
            }
        }
        state.set(badge);
        terminal.draw(|f| badge.draw(f)).unwrap();
        watchdog.feed(Duration::from_secs(5));
        ticker.next().await;
    }
}

extern crate alloc;
