//! Talk to the Ratatui Universe badge over USB serial, Bluetooth LE (`--ble`) or the Wi-Fi
//! relay (`--relay`).
//!
//! `cargo badge` opens a live mirror of the badge screen with its log; the arrow keys and
//! Enter drive the badge. The other subcommands are one-shot.
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::mpsc::{self, Receiver},
    thread,
    path::PathBuf,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::{
    Result,
    eyre::{OptionExt, WrapErr, bail},
};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph},
};

use link::Link;

mod flash;
mod link;

/// Mirror frames are wrapped in these, so log lines can't tear a frame (see ui/src/mirror.rs).
const FRAME_START: u8 = 0x02;
const FRAME_END: u8 = 0x03;
/// The badge's terminal is 53x24 cells.
const SCREEN: (u16, u16) = (53, 24);
const LOG_LINES: usize = 500;

#[derive(Parser)]
#[command(about)]
struct Cli {
    /// Serial port; by default the first one with the badge's USB IDs.
    #[arg(long, global = true, conflicts_with_all = ["ble", "relay"])]
    port: Option<String>,
    /// Connect over Bluetooth LE instead of USB.
    #[arg(long, global = true, conflicts_with = "relay")]
    ble: bool,
    /// Connect through the relay in badge.toml's [relay] section instead of USB.
    #[arg(long, global = true)]
    relay: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Live screen mirror and log (the default). Arrows and Enter drive the badge.
    Mirror,
    /// Follow the event and perf log.
    Log,
    /// Print the current screen as plain text.
    Screen,
    /// Press a button remotely.
    Key { key: Key },
    /// Start the booth demo.
    Demo,
    /// Start the idle scenes (the badge also does this when idle; see `[ambient]` in badge.toml).
    Ambient,
    /// Reboot into BOOTSEL for flashing (`cargo run --release` does this by itself).
    Bootsel,
    /// Update the firmware over the open link (USB, --ble or --relay), sending only the
    /// sectors that changed. Needs the A/B partition table (see README).
    /// Send one raw protocol line and print what comes back for half a second.
    Send { line: String },
    Flash {
        /// The firmware ELF.
        #[arg(default_value = "target/thumbv8m.main-none-eabihf/release/ratatui-badge")]
        elf: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::Up => "up",
            Key::Down => "down",
            Key::Left => "left",
            Key::Right => "right",
            Key::Select => "select",
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let port = if cli.ble {
        let (name, key) = ble_config();
        link::ble(&name, key)?
    } else if cli.relay {
        let (url, token) = relay_config()?;
        link::relay(&url, &token)?
    } else {
        link::serial(cli.port)?
    };
    match cli.command.unwrap_or(Command::Mirror) {
        Command::Mirror => {
            let terminal = ratatui::init();
            let result = Mirror::new(port)?.run(terminal);
            ratatui::restore();
            result
        }
        Command::Log => follow(port, None),
        Command::Screen => one_shot(port, "screen"),
        Command::Key { key } => one_shot(port, &format!("key {}", key.name())),
        Command::Demo => one_shot(port, "demo"),
        Command::Ambient => one_shot(port, "ambient"),
        Command::Bootsel => one_shot(port, "bootsel"),
        Command::Flash { .. } if cli.ble || cli.relay => bail!("updates only work over USB"),
        Command::Flash { elf } => flash::flash(port, &elf),
        Command::Send { line } => one_shot(port, &line),
    }
}

/// The badge's settings, from the same file the firmware build reads.
fn config() -> Result<toml::Table> {
    let path = std::env::var_os("BADGE_CONFIG").map_or_else(|| PathBuf::from("badge.toml"), PathBuf::from);
    let text = std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).wrap_err_with(|| format!("parsing {}", path.display()))
}

/// The name to look for and the key to prove, as the firmware was built with.
fn ble_config() -> (String, Option<String>) {
    let config = config().unwrap_or_default();
    let field = |key| config.get("ble").and_then(|ble| ble.get(key)).and_then(|v| v.as_str()).map(str::to_string);
    (field("name").unwrap_or_else(|| "Ratatui badge".to_string()), field("key"))
}

fn relay_config() -> Result<(String, String)> {
    let config = config()?;
    let relay = config.get("relay").ok_or_eyre("badge.toml has no [relay] section")?;
    let field = |key| relay.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Ok((field("url").ok_or_eyre("relay.url missing")?, field("token").ok_or_eyre("relay.token missing")?))
}

/// Write one line. The serial port times out writes after 50 ms, which is normal while the badge
/// is busy (erasing flash during an update), so keep going until a longer deadline.
fn send(port: &mut dyn Write, line: &str) -> Result<()> {
    let bytes = format!("{line}\n").into_bytes();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut written = 0;
    while written < bytes.len() {
        match port.write(&bytes[written..]) {
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut && Instant::now() < deadline => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn one_shot(mut port: Link, line: &str) -> Result<()> {
    send(port.writer.as_mut(), line)?;
    follow(port, Some(Duration::from_millis(500)))
}

/// Copy log output to stdout, skipping any mirror frames.
fn follow(port: Link, duration: Option<Duration>) -> Result<()> {
    let _writer = port.writer;
    let events = reader(port.reader);
    let mut stdout = std::io::stdout();
    let deadline = duration.map(|d| std::time::Instant::now() + d);
    loop {
        let wait = deadline.map_or(Duration::from_secs(3600), |d| d.saturating_duration_since(std::time::Instant::now()));
        match events.recv_timeout(wait) {
            Ok(Incoming::Line(line)) => writeln!(stdout, "{line}")?,
            Ok(Incoming::Frame(_)) => {}
            Ok(Incoming::Closed(error)) => bail!("badge disconnected: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("badge disconnected"),
        }
    }
}

enum Incoming {
    Line(String),
    Frame(Vec<u8>),
    Closed(String),
}

/// Split the stream into log lines and mirror frames on a background thread.
fn reader(mut port: Box<dyn Read + Send>) -> Receiver<Incoming> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut line, mut frame, mut in_frame) = (Vec::new(), Vec::new(), false);
        let mut buf = [0; 4096];
        loop {
            let n = match port.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    let _ = tx.send(Incoming::Closed(e.to_string()));
                    return;
                }
            };
            for &b in &buf[..n] {
                let event = match (in_frame, b) {
                    (_, FRAME_START) => {
                        (in_frame, frame) = (true, Vec::new());
                        None
                    }
                    (true, FRAME_END) => {
                        in_frame = false;
                        Some(Incoming::Frame(std::mem::take(&mut frame)))
                    }
                    (true, _) => {
                        frame.push(b);
                        None
                    }
                    (false, b'\n') => {
                        let text = String::from_utf8_lossy(&line).trim_end().to_string();
                        line.clear();
                        Some(Incoming::Line(text))
                    }
                    (false, _) => {
                        line.push(b);
                        None
                    }
                };
                if let Some(event) = event
                    && tx.send(event).is_err()
                {
                    return;
                }
            }
        }
    });
    rx
}

struct Mirror {
    port: Box<dyn Write + Send>,
    name: String,
    last_frame: Instant,
    last_request: Instant,
    events: Receiver<Incoming>,
    screen: Text<'static>,
    log: VecDeque<String>,
    perf: String,
    frames: u32,
    status: String,
}

impl Mirror {
    fn new(link: Link) -> Result<Self> {
        Ok(Self {
            port: link.writer,
            name: link.name,
            last_frame: Instant::now(),
            last_request: Instant::now(),
            events: reader(link.reader),
            screen: Text::from("waiting for the first frame…"),
            log: VecDeque::new(),
            perf: String::new(),
            frames: 0,
            status: String::new(),
        })
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        send(self.port.as_mut(), "mirror on")?;
        let result = self.event_loop(&mut terminal);
        let _ = send(self.port.as_mut(), "mirror off");
        result
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            while let Ok(incoming) = self.events.try_recv() {
                match incoming {
                    Incoming::Frame(bytes) => {
                        self.screen = bytes.into_text()?;
                        self.frames += 1;
                        self.last_frame = Instant::now();
                    }
                    Incoming::Line(line) if line.starts_with("perf ") => self.perf = line,
                    Incoming::Line(line) if line.is_empty() => {}
                    Incoming::Line(line) => {
                        self.log.push_back(line);
                        if self.log.len() > LOG_LINES {
                            self.log.pop_front();
                        }
                    }
                    Incoming::Closed(error) => bail!("badge disconnected: {error}"),
                }
            }
            // The badge forgets the request when a BLE or relay link drops and reconnects.
            if self.last_frame.elapsed() > Duration::from_secs(2) && self.last_request.elapsed() > Duration::from_secs(3) {
                send(self.port.as_mut(), "mirror on")?;
                self.last_request = Instant::now();
            }
            terminal.draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(16))? {
                continue;
            }
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let command = match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                KeyCode::Up => "key up",
                KeyCode::Down => "key down",
                KeyCode::Left => "key left",
                KeyCode::Right => "key right",
                KeyCode::Enter | KeyCode::Char(' ') => "key select",
                KeyCode::Char('d') => "demo",
                _ => continue,
            };
            self.status = command.to_string();
            send(self.port.as_mut(), command)?;
        }
    }

    fn draw(&self, f: &mut Frame) {
        let [main, help] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
        let screen_width = SCREEN.0 + 2;
        let [badge, log] = if main.width >= screen_width + 30 {
            Layout::horizontal([Constraint::Length(screen_width), Constraint::Fill(1)]).areas(main)
        } else {
            Layout::vertical([Constraint::Length(SCREEN.1 + 2), Constraint::Fill(1)]).areas(main)
        };
        let badge = Rect { height: badge.height.min(SCREEN.1 + 2), ..badge };
        let title = Line::from(vec![
            " badge ".bold(),
            Span::raw(format!("{} · {} frames ", self.name, self.frames)).dark_gray(),
        ]);
        f.render_widget(Paragraph::new(self.screen.clone()).block(Block::bordered().title(title)), badge);

        let block = Block::bordered().title(" log ".bold()).title_bottom(Line::from(self.perf.as_str()).dark_gray());
        let visible = block.inner(log).height as usize;
        let lines: Vec<Line> = self.log.iter().skip(self.log.len().saturating_sub(visible)).map(|l| Line::raw(l.as_str())).collect();
        f.render_widget(Paragraph::new(lines).block(block), log);

        let key = Style::new().fg(Color::Cyan).bold();
        let hints = [("←→", " page  "), ("↑↓", " change  "), ("enter", " select  "), ("d", " demo  "), ("q", " quit  ")];
        let mut spans: Vec<Span> = hints.iter().flat_map(|(k, v)| [Span::styled(*k, key), Span::raw(*v)]).collect();
        spans.push(Span::raw(self.status.as_str()).dark_gray());
        f.render_widget(Line::from(spans), help);
    }
}
