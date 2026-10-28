//! Encode the QR codes on the host so the badge only has to draw them.
use std::{env, fmt::Write, fs, path::PathBuf};

use qrcode::{EcLevel, QrCode};

const CODES: [(&str, &str); 2] = [
    ("QR_RATATUI_RS", "https://ratatui.rs"),
    ("QR_GITHUB", "https://github.com/ratatui/ratatui"),
];

fn main() {
    let mut out = String::new();
    for (name, url) in CODES {
        let code = QrCode::with_error_correction_level(url, EcLevel::M).unwrap();
        let width = code.width();
        let modules: Vec<bool> = code.to_colors().iter().map(|c| *c == qrcode::Color::Dark).collect();
        writeln!(out, "pub const {name}: crate::qr::Qr = crate::qr::Qr {{ width: {width}, modules: &{modules:?} }};").unwrap();
    }
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out_dir.join("qr_codes.rs"), out).unwrap();
    fs::write(out_dir.join("wordmark.rs"), wordmark()).unwrap();
}

/// The wordmark SVG is a grid of 10x10 rects: white letters with a gray drop shadow.
fn wordmark() -> String {
    const SVG: &str = "../assets/ratatui-wordmark.svg";
    println!("cargo:rerun-if-changed={SVG}");
    let svg = fs::read_to_string(SVG).unwrap();
    let mut grid = [[0u8; 36]; 5];
    for rect in svg.split("<rect").skip(1) {
        let attr = |name: &str| {
            let start = rect.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
            &rect[start..start + rect[start..].find('"').unwrap()]
        };
        let (x, y) = (attr("x").parse::<usize>().unwrap() / 10, attr("y").parse::<usize>().unwrap() / 10);
        grid[y][x] = if attr("fill") == "#ffffff" { 1 } else { 2 };
    }
    format!("pub const WORDMARK: [[u8; 36]; 5] = {grid:?};\n")
}
