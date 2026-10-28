//! Talk to the Ratatui Universe badge over its USB serial port.
//!
//! `cargo badge` opens a live mirror of the badge screen with its log; the arrow keys and
//! Enter drive the badge. The other subcommands are one-shot.
use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use ansi_to_tui::IntoText;
use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::{
    Result,
    eyre::{OptionExt, bail},
};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph},
};
use serialport::{SerialPort, SerialPortType};

/// The badge firmware uses the Pico SDK's USB IDs so `picotool -f` can find it.
const VID: u16 = 0x2E8A;
const PID: u16 = 0x0009;
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
    #[arg(long, global = true)]
    port: Option<String>,
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
    /// Reboot into BOOTSEL for flashing (`cargo run --release` does this by itself).
    Bootsel,
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
    let port = open(cli.port)?;
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
        Command::Bootsel => one_shot(port, "bootsel"),
    }
}

fn open(port: Option<String>) -> Result<Box<dyn SerialPort>> {
    let name = match port {
        Some(name) => name,
        None => serialport::available_ports()?
            .into_iter()
            .find(|p| matches!(&p.port_type, SerialPortType::UsbPort(usb) if usb.vid == VID && usb.pid == PID))
            // macOS lists each port as tty.* and cu.*; cu.* doesn't wait for carrier detect.
            .map(|p| p.port_name.replace("/dev/tty.", "/dev/cu."))
            .ok_or_eyre("badge not found: is it plugged in and running the Ratatui firmware?")?,
    };
    let mut port = serialport::new(&name, 115_200).timeout(Duration::from_millis(50)).open()?;
    // The badge only talks once the host raises DTR, as terminal programs do.
    port.write_data_terminal_ready(true)?;
    Ok(port)
}

fn send(port: &mut dyn SerialPort, line: &str) -> Result<()> {
    port.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

fn one_shot(mut port: Box<dyn SerialPort>, line: &str) -> Result<()> {
    send(port.as_mut(), line)?;
    follow(port, Some(Duration::from_millis(500)))
}

/// Copy log output to stdout, skipping any mirror frames.
fn follow(port: Box<dyn SerialPort>, duration: Option<Duration>) -> Result<()> {
    let events = reader(port);
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

/// Split the serial stream into log lines and mirror frames on a background thread.
fn reader(mut port: Box<dyn SerialPort>) -> Receiver<Incoming> {
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
    port: Box<dyn SerialPort>,
    events: Receiver<Incoming>,
    screen: Text<'static>,
    log: VecDeque<String>,
    perf: String,
    frames: u32,
    status: String,
}

impl Mirror {
    fn new(port: Box<dyn SerialPort>) -> Result<Self> {
        let events = reader(port.try_clone()?);
        Ok(Self {
            port,
            events,
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
        let title = Line::from(vec![" badge ".bold(), Span::raw(format!("{} frames ", self.frames)).dark_gray()]);
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
