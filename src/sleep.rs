//! Hold RESET for a second to sleep, as the stock firmware does; any front button wakes the
//! badge. RESET is also wired to GPIO14, so at boot we can tell it's still held.
//!
//! Ported from Pimoroni's `modules/c/powman/powman.c` and the Pico SDK's `hardware_powman`.
use core::ptr::write_volatile;

use embassy_rp::{
    gpio::{Input, Pull},
    pac,
    peripherals::PIN_14,
    Peri,
};
use embassy_time::{Duration, block_for};

use crate::leds::Leds;

/// The front buttons' shared interrupt line (pulled up, low while any button is held).
const BUTTON_INT: usize = 15;
/// Pins that keep their pulls while asleep: the front buttons and their interrupt line.
const BUTTONS: [usize; 6] = [6, 7, 9, 10, 11, BUTTON_INT];
/// RESET (14), BOOT (22), POWER_EN (41) and the battery and 1V1 sense pins (40, 42) float.
const FLOATING: [usize; 5] = [14, 22, 40, 41, 42];

/// If this boot came from a RESET press that's still held, fade the case LEDs and go to
/// sleep. Letting go before the fade ends boots as normal.
pub fn check(reset: Peri<'static, PIN_14>, leds: &mut Leds) {
    let chip_reset = pac::POWMAN.chip_reset().read();
    let reset = Input::new(reset, Pull::Up);
    if !chip_reset.had_run_low() || reset.is_high() {
        return;
    }
    // A swoosh across the four LEDs, then fade out together, about a second in all.
    for step in 0..200u32 {
        if reset.is_high() {
            leds.set([0; 4]);
            return;
        }
        let level = |i: u32| {
            let t = step.saturating_sub(i * 15);
            (if t < 100 { t * 255 / 100 } else { 255u32.saturating_sub((t - 100) * 255 / 40) }) as u8
        };
        let order = [1, 2, 3, 0]; // left to right, as the stock firmware does
        let mut levels = [0; 4];
        for (i, led) in order.into_iter().enumerate() {
            levels[led] = level(i as u32);
        }
        leds.set(levels);
        block_for(Duration::from_millis(5));
    }
    leds.set([0; 4]);
    unsafe { power_off() }
}

const PASSWORD: u32 = 0x5AFE << 16;

#[inline(always)]
unsafe fn powman_write(reg: *mut u32, value: u32) {
    unsafe { write_volatile(reg, PASSWORD | value) }
}
#[inline(always)]
unsafe fn powman_set(reg: *mut u32, bits: u32) {
    unsafe { write_volatile(reg.byte_add(0x2000), PASSWORD | bits) }
}
#[inline(always)]
unsafe fn powman_clear(reg: *mut u32, bits: u32) {
    unsafe { write_volatile(reg.byte_add(0x3000), PASSWORD | bits) }
}

/// Power off the switched core (power state P1.7) until a front button is pressed. Waking
/// is a fresh boot.
unsafe fn power_off() -> ! {
    cortex_m::interrupt::disable();
    let powman = pac::POWMAN;

    // Park every pin for low leakage: no output, input buffer off, pulled down. The buttons
    // keep their pull-ups so the interrupt line can wake us.
    for pin in 0..48 {
        pac::IO_BANK0.gpio(pin).ctrl().write(|w| w.set_funcsel(0x1f));
        pac::PADS_BANK0.gpio(pin).modify(|w| {
            let button = BUTTONS.contains(&pin);
            w.set_ie(button);
            w.set_pue(button);
            w.set_pde(!button && !FLOATING.contains(&pin));
        });
    }

    unsafe {
        powman_set(powman.vreg_ctrl().as_ptr() as _, 1 << 13); // UNLOCK
        powman_write(powman.dbg_pwrcfg().as_ptr() as _, 1); // power down even with a debugger

        // Wake on a falling edge of the button interrupt line.
        let pwrup = powman.pwrup(3).as_ptr() as *mut u32;
        powman_write(pwrup, 1 << 8 | BUTTON_INT as u32); // edge mode, active low
        powman_clear(pwrup, 1 << 9); // STATUS: drop any latched edge
        powman_set(pwrup, 1 << 6); // ENABLE

        // Sleep with SRAM off and wake with it off too; the sequencer leaves both banks be.
        powman_set(powman.seq_cfg().as_ptr() as _, 0b11); // HW_PWRUP_SRAM0 | HW_PWRUP_SRAM1
        for i in 0..4 {
            write_volatile(powman.boot(i).as_ptr(), 0); // wake into a normal boot
        }
    }
    enter_p1_7()
}

/// Runs from RAM: flash goes away with the switched core.
#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn enter_p1_7() -> ! {
    let state = pac::POWMAN.state().as_ptr() as *mut u32;
    unsafe {
        powman_clear(state, 1 << 8); // REQ_IGNORED
        powman_write(state, 0xf << 4); // request every domain off (REQ is inverted)
    }
    loop {
        cortex_m::asm::wfi();
    }
}
