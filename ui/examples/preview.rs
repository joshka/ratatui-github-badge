//! Render each page through Mousefood on the host and save PNGs:
//! `cargo run -p badge-ui --example preview --target aarch64-apple-darwin -- /tmp/badge-preview`
use std::{convert::Infallible, fs::File, io::BufWriter, path::PathBuf};

use badge_ui::{App, CELL_PX, Image, Key, Sensors};
use ratatui::layout::Rect;
use mousefood::{
    embedded_graphics::{pixelcolor::Rgb888, prelude::*},
    prelude::*,
};
use ratatui::Terminal;

const W: usize = 320;
const H: usize = 240;

struct Screen(Vec<Rgb888>);

impl OriginDimensions for Screen {
    fn size(&self) -> Size {
        Size::new(W as u32, H as u32)
    }
}

impl DrawTarget for Screen {
    type Color = Rgb888;
    type Error = Infallible;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(&mut self, pixels: I) -> Result<(), Infallible> {
        for Pixel(p, c) in pixels {
            if (0..W as i32).contains(&p.x) && (0..H as i32).contains(&p.y) {
                self.0[p.y as usize * W + p.x as usize] = c;
            }
        }
        Ok(())
    }
}

/// Draw the image slots the way the firmware does after Mousefood flushes.
fn with_images(screen: &Screen, images: &[(Image, Rect)]) -> Screen {
    let mut out = Screen(screen.0.clone());
    for &(image, rect) in images {
        let (cw, ch) = (CELL_PX.0 as usize, CELL_PX.1 as usize);
        let (w, h) = (rect.width as usize * cw, rect.height as usize * ch);
        for y in 0..h {
            for x in 0..w {
                let p = image.slot_pixel(w as u32, h as u32, x as u32, y as u32) as u32;
                let rgb = Rgb888::new(((p >> 11) << 3) as u8, (((p >> 5) & 0x3F) << 2) as u8, ((p & 0x1F) << 3) as u8);
                out.0[(rect.y as usize * ch + y) * W + rect.x as usize * cw + x] = rgb;
            }
        }
    }
    out
}

fn save(screen: &Screen, path: PathBuf) {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(&path).unwrap()), W as u32 * 2, H as u32 * 2);
    encoder.set_color(png::ColorType::Rgb);
    let mut data = Vec::with_capacity(W * H * 12);
    for y in 0..H * 2 {
        for x in 0..W * 2 {
            let c = screen.0[(y / 2) * W + x / 2];
            data.extend([c.r(), c.g(), c.b()]);
        }
    }
    encoder.write_header().unwrap().write_image_data(&data).unwrap();
    println!("{}", path.display());
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or("/tmp/badge-preview".into()));
    std::fs::create_dir_all(&dir).unwrap();
    let sensors = Sensors {
        accel: [0.15, 0.95, 0.2],
        imu_ok: true,
        battery_v: 3.95,
        light: 0.45,
        touch_ok: true,
        touch: 0b0000_0100,
        frame_ms: 38,
        flush_ms: 22,
        heap_used: 48_000,
        heap_free: 110_000,
        ..Default::default()
    };
    let mut screen = Screen(vec![Rgb888::BLACK; W * H]);
    let mut app = App::new();
    let mut terminal = Terminal::new(EmbeddedBackend::new(&mut screen, EmbeddedBackendConfig::default())).unwrap();
    let names = ["ratatui", "live", "maze", "widgets", "get", "me", "debug"];
    for (page, name) in names.iter().enumerate() {
        // Let entry effects finish and the history fill.
        for i in 0..60 {
            let mut s = sensors;
            s.accel[0] = 0.4 * ((i as f32) * 0.2).sin();
            app.update(33, s);
            terminal.draw(|f| app.draw(f)).unwrap();
        }
        save(&with_images(terminal.backend().display(), &app.images), dir.join(format!("{page}-{name}.png")));
        app.key(Key::Right);
    }
    for (page, name) in [(7, "billboard"), (8, "life"), (9, "plasma"), (10, "rain"), (11, "warp")] {
        app.goto_page(page);
        for _ in 0..70 {
            app.update(33, sensors);
            terminal.draw(|f| app.draw(f)).unwrap();
        }
        save(terminal.backend().display(), dir.join(format!("{page}-{name}.png")));
    }
    println!("leds {:?}", app.leds());
}
