//! Turn the per-user `badge.toml` (or `badge.example.toml`) into Rust, and encode QR codes
//! and the avatar on the host so the badge only has to draw them.
use std::{
    collections::BTreeMap,
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

use qrcode::{EcLevel, QrCode};
use serde::Deserialize;

const PROJECT_CODES: [(&str, &str); 2] = [
    ("QR_RATATUI_RS", "https://ratatui.rs"),
    ("QR_GITHUB", "https://github.com/ratatui/ratatui"),
];
/// The avatar is drawn as a square this many pixels wide.
const AVATAR_SIDE: u32 = 88;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    name: String,
    handle: String,
    role: String,
    #[serde(default)]
    ambient: Ambient,
    moods: Vec<String>,
    avatar: Option<PathBuf>,
    contacts: Vec<Contact>,
    #[serde(default = "conference_wifi")]
    wifi: Vec<Wifi>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wifi {
    ssid: String,
    password: Option<String>,
}

/// GitHub Universe's attendee network; its password is shared with everyone at the event.
fn conference_wifi() -> Vec<Wifi> {
    vec![Wifi { ssid: "GitHub Events".into(), password: Some("HelloWorld".into()) }]
}

/// Ambient mode's scenes, in the order the UI expects their weights.
const SCENES: [&str; 4] = ["fire", "plasma", "rain", "warp"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Ambient {
    idle_seconds: u32,
    scene_seconds: u32,
    weights: BTreeMap<String, u32>,
}

impl Default for Ambient {
    fn default() -> Self {
        Self { idle_seconds: 30, scene_seconds: 8, weights: BTreeMap::new() }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Contact {
    label: String,
    qr: String,
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut out = String::new();
    for (name, text) in PROJECT_CODES {
        writeln!(out, "pub const {name}: crate::qr::Qr = {};", qr(text)).unwrap();
    }
    fs::write(out_dir.join("qr_codes.rs"), out).unwrap();
    fs::write(out_dir.join("config.rs"), config(&out_dir)).unwrap();
}

fn qr(text: &str) -> String {
    let code = QrCode::with_error_correction_level(text, EcLevel::M).unwrap();
    let modules: Vec<bool> = code.to_colors().iter().map(|c| *c == qrcode::Color::Dark).collect();
    format!("crate::qr::Qr {{ width: {}, modules: &{modules:?} }}", code.width())
}

/// `$BADGE_CONFIG`, else `badge.toml` at the repo root, else the committed example.
fn config(out_dir: &Path) -> String {
    println!("cargo:rerun-if-env-changed=BADGE_CONFIG");
    println!("cargo:rerun-if-changed=../badge.toml");
    println!("cargo:rerun-if-changed=../badge.example.toml");
    let path = match env::var_os("BADGE_CONFIG") {
        Some(path) => PathBuf::from(path),
        None if Path::new("../badge.toml").exists() => PathBuf::from("../badge.toml"),
        None => {
            println!("cargo:warning=no badge.toml; using badge.example.toml (copy it to badge.toml and edit)");
            PathBuf::from("../badge.example.toml")
        }
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let config: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(!config.moods.is_empty(), "{}: needs at least one mood", path.display());
    assert!(!config.contacts.is_empty(), "{}: needs at least one contact", path.display());

    let mut out = String::new();
    let Config { name, handle, role, moods, .. } = &config;
    writeln!(out, "pub const NAME: &str = {name:?};").unwrap();
    writeln!(out, "pub const HANDLE: &str = {handle:?};").unwrap();
    writeln!(out, "pub const ROLE: &str = {role:?};").unwrap();
    writeln!(out, "pub const MOODS: &[&str] = &{moods:?};").unwrap();
    let Ambient { idle_seconds, scene_seconds, weights } = &config.ambient;
    for scene in weights.keys() {
        assert!(SCENES.contains(&scene.as_str()), "{}: unknown ambient scene {scene:?}; expected one of {SCENES:?}", path.display());
    }
    // Rain by default shows up three times as often as each of the others.
    let weights = SCENES.map(|s| weights.get(s).copied().unwrap_or(if s == "rain" { 3 } else { 1 }));
    assert!(weights.iter().any(|w| *w > 0), "{}: every ambient weight is 0", path.display());
    writeln!(out, "pub const AMBIENT_IDLE_MS: u32 = {};", idle_seconds * 1000).unwrap();
    writeln!(out, "pub const AMBIENT_SCENE_MS: u32 = {};", scene_seconds.max(&1) * 1000).unwrap();
    writeln!(out, "pub const AMBIENT_WEIGHTS: [u32; 4] = {weights:?};").unwrap();
    writeln!(out, "pub const CONTACTS: &[Contact] = &[").unwrap();
    for Contact { label, qr: text } in &config.contacts {
        let width = QrCode::with_error_correction_level(text, EcLevel::M).unwrap().width();
        if width + 8 > 132 / 2 {
            println!("cargo:warning=QR for {label:?} is {width} modules; under 2 px each it may not scan. Shorten it.");
        }
        writeln!(out, "    Contact {{ label: {label:?}, qr: {} }},", qr(text)).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "pub const WIFI: &[Wifi] = &[").unwrap();
    for Wifi { ssid, password } in &config.wifi {
        writeln!(out, "    Wifi {{ ssid: {ssid:?}, password: {password:?} }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "pub const AVATAR_SIDE: u32 = {AVATAR_SIDE};").unwrap();
    match &config.avatar {
        Some(avatar) => {
            let avatar = resolve(&path, avatar);
            println!("cargo:rerun-if-changed={}", avatar.display());
            fs::write(out_dir.join("avatar.rgb565"), rgb565_square(&avatar)).unwrap();
            writeln!(out, "pub const AVATAR: Option<&[u8]> = Some(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/avatar.rgb565\")));").unwrap();
        }
        None => writeln!(out, "pub const AVATAR: Option<&[u8]> = None;").unwrap(),
    }
    out
}

/// Expand `~/` and resolve relative paths against the config file's directory.
fn resolve(config: &Path, path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        return PathBuf::from(env::var_os("HOME").unwrap()).join(rest);
    }
    config.parent().unwrap().join(path)
}

/// Center-crop to a square, scale, and flatten transparency onto the UI background.
fn rgb565_square(path: &Path) -> Vec<u8> {
    let fail = |e: &dyn std::fmt::Display| -> ! { panic!("avatar {} (PNG or JPEG): {e}", path.display()) };
    let image = image::ImageReader::open(path).unwrap_or_else(|e| fail(&e));
    let image = image.with_guessed_format().unwrap_or_else(|e| fail(&e)).decode().unwrap_or_else(|e| fail(&e));
    let image = image.resize_to_fill(AVATAR_SIDE, AVATAR_SIDE, image::imageops::FilterType::Lanczos3).into_rgba8();
    let mut out = Vec::with_capacity((AVATAR_SIDE * AVATAR_SIDE * 2) as usize);
    for image::Rgba([r, g, b, a]) in image.pixels() {
        let blend = |c: u8, bg: u32| (c as u32 * *a as u32 + bg * (255 - *a as u32)) / 255;
        let (r, g, b) = (blend(*r, 10), blend(*g, 12), blend(*b, 24));
        out.extend((((r >> 3) << 11 | (g >> 2) << 5 | b >> 3) as u16).to_be_bytes());
    }
    out
}
