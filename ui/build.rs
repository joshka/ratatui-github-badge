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
    let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("qr_codes.rs");
    fs::write(path, out).unwrap();
}
