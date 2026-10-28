//! ST7789 on the badge's 8-bit parallel bus, driven by bit-banging SIO registers.
//!
//! Pin map (from badger/home `hardware/README.md`):
//! CS=27 DC=28 WR=30 RD=31 D0..D7=32..39 BL=26 POWER_EN=41.
//!
//! The setup sequence is copied from Pimoroni's `modules/c/st7789/st7789.cpp` in
//! pimoroni/tufty2350. The panel is natively 240x320 (portrait), so a 320x240
//! landscape frame is sent one landscape column (240 pixels) at a time.

use core::convert::Infallible;

use embassy_rp::gpio::Output;
use embassy_rp::pac;
use embassy_time::{Duration, block_for};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 240;

const WR_MASK: u32 = 1 << 30;

// WR timing at 200 MHz (5 ns/cycle). The datasheet asks for >= 15 ns low, >= 15 ns high
// and a >= 66 ns cycle; with loop overhead this stays around 100 ns per byte.
const WR_LOW_CYCLES: u32 = 8;
const WR_HIGH_CYCLES: u32 = 8;

#[inline(always)]
fn delay<const N: u32>() {
    for _ in 0..N {
        cortex_m::asm::nop();
    }
}

/// Owns the bus pins and a full-resolution RGB565 framebuffer.
pub struct Lcd {
    cs: Output<'static>,
    dc: Output<'static>,
    // Held so the pins stay configured as SIO outputs; data and WR are written via registers.
    _wr: Output<'static>,
    _rd: Output<'static>,
    _data: [Output<'static>; 8],
    _bl: Output<'static>,
    _power: Output<'static>,
    fb: &'static mut [u16; WIDTH * HEIGHT],
}

#[allow(clippy::too_many_arguments)]
impl Lcd {
    pub fn new(
        cs: Output<'static>,
        dc: Output<'static>,
        wr: Output<'static>,
        rd: Output<'static>,
        data: [Output<'static>; 8],
        mut bl: Output<'static>,
        mut power: Output<'static>,
        fb: &'static mut [u16; WIDTH * HEIGHT],
    ) -> Self {
        // The controller sits on the switched 3V3 rail and ignores commands until it is on.
        power.set_high();
        bl.set_low();
        block_for(Duration::from_millis(50));

        let mut lcd = Self {
            cs,
            dc,
            _wr: wr,
            _rd: rd,
            _data: data,
            _bl: bl,
            _power: power,
            fb,
        };
        lcd.init();
        lcd
    }

    fn init(&mut self) {
        self.command(0x01, &[]); // SWRESET
        block_for(Duration::from_millis(150));
        self.command(0x3A, &[0x05]); // COLMOD: 16 bpp
        self.command(0xB2, &[0x0c, 0x0c, 0x00, 0x33, 0x33]); // PORCTRL
        self.command(0xC0, &[0x2c]); // LCMCTRL
        self.command(0xC2, &[0x01]); // VDVVRHEN
        self.command(0xC3, &[0x0f]); // VRHS
        self.command(0xC4, &[0x20]); // VDVS
        self.command(0xD0, &[0xa4, 0xa1]); // PWCTRL1
        self.command(0xC6, &[0x0f]); // FRCTRL2
        self.command(0xB0, &[0x00, 0xc0]); // RAMCTRL
        self.command(0xB7, &[0x35]); // GCTRL
        self.command(0xBB, &[0x1b]); // VCOMS
        self.command(
            0xE0,
            &[
                0xF0, 0x00, 0x06, 0x04, 0x05, 0x05, 0x31, 0x44, 0x48, 0x36, 0x12, 0x12, 0x2B, 0x34,
            ],
        );
        self.command(
            0xE1,
            &[
                0xF0, 0x0B, 0x0F, 0x0F, 0x0D, 0x26, 0x31, 0x43, 0x47, 0x38, 0x14, 0x14, 0x2C, 0x32,
            ],
        );
        self.command(0x21, &[]); // INVON
        self.command(0x11, &[]); // SLPOUT
        block_for(Duration::from_millis(100));
        self.command(0x2A, &[0, 0, 0, 239]); // CASET
        self.command(0x2B, &[0, 0, 0x01, 0x3f]); // RASET 0..319
        self.command(0x36, &[0x80 | 0x10]); // MADCTL: ROW_ORDER | SCAN_ORDER
        self.fb.fill(0);
        self.flush(false);
        self.command(0x29, &[]); // DISPON
        self._bl.set_high();
    }

    fn command(&mut self, cmd: u8, args: &[u8]) {
        self.cs.set_low();
        self.dc.set_low();
        write_byte(cmd);
        self.dc.set_high();
        for &b in args {
            write_byte(b);
        }
        self.cs.set_high();
    }

    /// Send the whole framebuffer to the panel, rotated 180 degrees when `flipped`.
    pub fn flush(&mut self, flipped: bool) {
        self.cs.set_low();
        self.dc.set_low();
        self.cs.set_high();
        self.command(0x2A, &[0, 0, 0, 239]); // CASET
        self.command(0x2B, &[0, 0, 0x01, 0x3f]); // RASET 0..319
        self.cs.set_low();
        self.dc.set_low();
        write_byte(0x2C); // RAMWR
        self.dc.set_high();
        for x in 0..WIDTH {
            for y in 0..HEIGHT {
                let i = y * WIDTH + x;
                let px = self.fb[if flipped { WIDTH * HEIGHT - 1 - i } else { i }];
                write_byte((px >> 8) as u8);
                write_byte(px as u8);
            }
        }
        self.cs.set_high();
    }
}

/// D0..D7 are GPIO32..39, i.e. bits 0..7 of the SIO high bank.
#[inline(always)]
fn write_byte(b: u8) {
    let sio = pac::SIO;
    sio.gpio_out(1).value_clr().write_value(0xff);
    sio.gpio_out(1).value_set().write_value(b as u32);
    sio.gpio_out(0).value_clr().write_value(WR_MASK);
    delay::<WR_LOW_CYCLES>();
    sio.gpio_out(0).value_set().write_value(WR_MASK);
    delay::<WR_HIGH_CYCLES>();
}

impl OriginDimensions for Lcd {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Lcd {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if (0..WIDTH as i32).contains(&p.x) && (0..HEIGHT as i32).contains(&p.y) {
                self.fb[p.y as usize * WIDTH + p.x as usize] = RawU16::from(c).into_inner();
            }
        }
        Ok(())
    }

    fn fill_solid(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        color: Self::Color,
    ) -> Result<(), Self::Error> {
        let area = area.intersection(&self.bounding_box());
        let Some(br) = area.bottom_right() else {
            return Ok(());
        };
        let raw = RawU16::from(color).into_inner();
        for y in area.top_left.y..=br.y {
            let row = y as usize * WIDTH;
            self.fb[row + area.top_left.x as usize..=row + br.x as usize].fill(raw);
        }
        Ok(())
    }

    /// Glyphs arrive here; writing rows directly avoids per-pixel bounds checks.
    fn fill_contiguous<I>(
        &mut self,
        area: &embedded_graphics::primitives::Rectangle,
        colors: I,
    ) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let clipped = area.intersection(&self.bounding_box());
        if clipped != *area {
            return self.draw_iter(area.points().zip(colors).map(|(p, c)| Pixel(p, c)));
        }
        let mut colors = colors.into_iter();
        let (x0, w) = (area.top_left.x as usize, area.size.width as usize);
        for y in area.top_left.y as usize..area.top_left.y as usize + area.size.height as usize {
            for px in &mut self.fb[y * WIDTH + x0..y * WIDTH + x0 + w] {
                match colors.next() {
                    Some(c) => *px = RawU16::from(c).into_inner(),
                    None => return Ok(()),
                }
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.fb.fill(RawU16::from(color).into_inner());
        Ok(())
    }
}
