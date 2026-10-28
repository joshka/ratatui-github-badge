//! Talk to the Ratatui Universe badge over USB serial, Bluetooth LE (`--ble`) or the Wi-Fi
//! relay (`--relay`).
//!
//! `cargo badge` opens a live mirror of the badge screen with its log; the arrow keys and
//! Enter drive the badge. The other subcommands are one-shot.
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
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

/// Opens a fresh link of the kind picked on the command line, so the mirror and the log can
/// come back after the badge reboots, is reflashed or drops off the air.
type Connect = Arc<dyn Fn() -> Result<Link> + Send + Sync>;

/// How long to wait between attempts to reach the badge again.
const RETRY: Duration = Duration::from_secs(1);
/// The badge logs a perf line every 2 s, so this much silence means the link is dead even if
/// the transport hasn't noticed.
const SILENCE: Duration = Duration::from_secs(15);

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let connect: Connect = if cli.ble {
        let (name, key) = ble_config();
        Arc::new(move || link::ble(&name, key.clone()))
    } else if cli.relay {
        let (url, token) = relay_config()?;
        Arc::new(move || link::relay(&url, &token))
    } else {
        let port = cli.port;
        Arc::new(move || link::serial(port.clone()))
    };
    match cli.command.unwrap_or(Command::Mirror) {
        Command::Mirror => {
            let terminal = ratatui::init();
            let result = Mirror::new(connect).run(terminal);
            ratatui::restore();
            result
        }
        Command::Log => follow_forever(&connect),
        Command::Screen => one_shot(connect()?, "screen"),
        Command::Key { key } => one_shot(connect()?, &format!("key {}", key.name())),
        Command::Demo => one_shot(connect()?, "demo"),
        Command::Ambient => one_shot(connect()?, "ambient"),
        Command::Bootsel => one_shot(connect()?, "bootsel"),
        Command::Flash { .. } if cli.ble || cli.relay => bail!("updates only work over USB"),
        Command::Flash { elf } => flash::flash(connect()?, &elf),
        Command::Send { line } => one_shot(connect()?, &line),
    }
}

enum Attempt {
    Failed(String),
    Connected(Link),
}

/// Keep trying to open a link on a background thread until one opens.
fn reconnect(connect: &Connect) -> Receiver<Attempt> {
    let (tx, rx) = mpsc::channel();
    let connect = connect.clone();
    thread::spawn(move || {
        loop {
            match connect() {
                Ok(link) => {
                    let _ = tx.send(Attempt::Connected(link));
                    return;
                }
                Err(error) => {
                    if tx.send(Attempt::Failed(format!("{error:#}"))).is_err() {
                        return;
                    }
                }
            }
            thread::sleep(RETRY);
        }
    });
    rx
}

/// Set by Ctrl-C while `log` has a link open, so it can hang up before exiting.
static STOP: AtomicBool = AtomicBool::new(false);
static FOLLOWING: AtomicBool = AtomicBool::new(false);

/// `cargo badge log`: follow the log across reboots and reflashes.
fn follow_forever(connect: &Connect) -> Result<()> {
    // Without a link there's nothing to hang up, so exit straight away.
    ctrlc::set_handler(|| {
        if FOLLOWING.load(Ordering::SeqCst) {
            STOP.store(true, Ordering::SeqCst);
        } else {
            std::process::exit(130);
        }
    })?;
    let mut last_error = String::new();
    loop {
        match connect() {
            Ok(link) => {
                if !last_error.is_empty() {
                    eprintln!("-- connected to {}", link.name);
                    last_error.clear();
                }
                FOLLOWING.store(true, Ordering::SeqCst);
                let result = follow(link, None);
                FOLLOWING.store(false, Ordering::SeqCst);
                if STOP.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if let Err(error) = result {
                    eprintln!("-- {error}; reconnecting…");
                    last_error = error.to_string();
                }
            }
            Err(error) => {
                let error = format!("{error:#}");
                if error != last_error {
                    eprintln!("-- {error}; retrying…");
                    last_error = error;
                }
            }
        }
        thread::sleep(RETRY);
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
/// Copy log output to stdout, skipping any mirror frames. Without a duration, follow until
/// the link drops or goes silent.
fn follow(port: Link, duration: Option<Duration>) -> Result<()> {
    let events = reader(port.reader);
    let result = copy_log(&events, duration);
    Link::close(port.writer, port.closed);
    result
}

fn copy_log(events: &Receiver<Incoming>, duration: Option<Duration>) -> Result<()> {
    let mut stdout = std::io::stdout();
    let deadline = duration.map(|d| Instant::now() + d);
    let mut last_data = Instant::now();
    loop {
        if STOP.load(Ordering::SeqCst) || deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(());
        }
        if deadline.is_none() && last_data.elapsed() > SILENCE {
            bail!("no data from the badge for {}s", SILENCE.as_secs());
        }
        // Wake often enough to notice Ctrl-C.
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(Incoming::Line(line)) => writeln!(stdout, "{line}")?,
            Ok(Incoming::Frame(_)) => {}
            Ok(Incoming::Closed(error)) => bail!("badge disconnected: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("badge disconnected"),
        }
        last_data = Instant::now();
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
    connect: Connect,
    /// The open link's writer and incoming stream, or `None` while reconnecting.
    port: Option<(Box<dyn Write + Send>, Receiver<Incoming>)>,
    /// See [`Link::closed`].
    closed: Option<Receiver<()>>,
    /// When anything last arrived, to spot a link that died without an error.
    last_data: Instant,
    /// Set while a background thread is trying to reach the badge.
    connecting: Option<Receiver<Attempt>>,
    /// Why the last attempt failed, shown while reconnecting.
    link_error: String,
    name: String,
    last_frame: Instant,
    last_request: Instant,
    screen: Text<'static>,
    log: VecDeque<String>,
    perf: String,
    frames: u32,
    status: String,
}

impl Mirror {
    fn new(connect: Connect) -> Self {
        Self {
            connecting: Some(reconnect(&connect)),
            connect,
            port: None,
            closed: None,
            last_data: Instant::now(),
            link_error: String::new(),
            name: "connecting…".to_string(),
            last_frame: Instant::now(),
            last_request: Instant::now(),
            screen: Text::from("looking for the badge…"),
            log: VecDeque::new(),
            perf: String::new(),
            frames: 0,
            status: String::new(),
        }
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let result = self.event_loop(&mut terminal);
        if let Some((mut writer, _)) = self.port.take() {
            let _ = send(writer.as_mut(), "mirror off");
            Link::close(writer, self.closed.take());
        }
        result
    }

    fn push_log(&mut self, line: String) {
        self.log.push_back(line);
        if self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    /// Drop the link and start looking for the badge again, keeping the log and last screen.
    fn disconnected(&mut self, reason: &str) {
        // Hang up properly, or a Bluetooth badge won't advertise for us again for a while.
        if let Some((writer, _)) = self.port.take() {
            Link::close(writer, self.closed.take());
        }
        self.push_log(format!("-- disconnected: {reason}"));
        self.link_error.clear();
        self.connecting = Some(reconnect(&self.connect));
    }

    fn poll_connecting(&mut self) {
        let Some(connecting) = &self.connecting else { return };
        while let Ok(attempt) = connecting.try_recv() {
            match attempt {
                Attempt::Failed(error) => self.link_error = error,
                Attempt::Connected(link) => {
                    self.connecting = None;
                    self.name = link.name;
                    self.port = Some((link.writer, reader(link.reader)));
                    self.closed = link.closed;
                    self.last_data = Instant::now();
                    self.push_log(format!("-- connected to {}", self.name));
                    self.link_error.clear();
                    self.last_frame = Instant::now();
                    self.command("mirror on");
                    return;
                }
            }
        }
    }

    /// Send a line to the badge; a failed write means the link has gone.
    fn command(&mut self, line: &str) {
        let Some((writer, _)) = &mut self.port else {
            self.status = format!("{line}: not connected");
            return;
        };
        if let Err(error) = send(writer.as_mut(), line) {
            self.disconnected(&error.to_string());
        }
        self.last_request = Instant::now();
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.poll_connecting();
            while let Some(incoming) = self.port.as_ref().and_then(|(_, events)| events.try_recv().ok()) {
                self.last_data = Instant::now();
                match incoming {
                    Incoming::Frame(bytes) => {
                        self.screen = bytes.into_text()?;
                        self.frames += 1;
                        self.last_frame = Instant::now();
                    }
                    Incoming::Line(line) if line.starts_with("perf ") => self.perf = line,
                    Incoming::Line(line) if line.is_empty() => {}
                    Incoming::Line(line) => self.push_log(line),
                    Incoming::Closed(error) => self.disconnected(&error),
                }
            }
            if self.port.is_some() && self.last_data.elapsed() > SILENCE {
                self.disconnected(&format!("no data for {}s", SILENCE.as_secs()));
            }
            // The badge forgets the request when it reboots or a BLE or relay link drops.
            if self.port.is_some()
                && self.last_frame.elapsed() > Duration::from_secs(2)
                && self.last_request.elapsed() > Duration::from_secs(3)
            {
                self.command("mirror on");
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
            self.command(command);
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
        let link = if self.port.is_some() {
            Span::raw(format!("{} · {} frames ", self.name, self.frames)).dark_gray()
        } else if self.link_error.is_empty() {
            Span::raw("reconnecting… ").yellow()
        } else {
            Span::raw(format!("reconnecting… ({}) ", self.link_error)).yellow()
        };
        let title = Line::from(vec![" badge ".bold(), link]);
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
