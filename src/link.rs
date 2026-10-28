//! The text protocol the host tools speak, over any transport: USB serial, Bluetooth LE or the
//! relay. Each transport has an [`Outbox`] for bytes to the host and feeds command lines back
//! through [`Outbox::command`].
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, pipe::Pipe};

/// Commands for the main loop. Mirroring is per transport, so it is handled here instead.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 8> = Channel::new();
/// Firmware update lines for the main loop (see `ota.rs`), which owns the flash. Transports
/// wait for room, so a fast host is slowed down rather than losing data.
pub static OTA: Channel<CriticalSectionRawMutex, (Source, heapless::String<LINE>), 4> = Channel::new();

pub static USB: Outbox<32768> = Outbox::new(Source::Usb);
// Small, to leave the heap room: these links are too slow for big mirror frames anyway, and
// skip any frame that doesn't fit.
pub static BLE: Outbox<8192> = Outbox::new(Source::Ble);
pub static RELAY: Outbox<8192> = Outbox::new(Source::Relay);

/// The longest line from the host: an `ota data` line carries 192 bytes as base64.
pub const LINE: usize = 300;

/// Which link a line came from, so replies go back the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Usb,
    Ble,
    Relay,
}

/// Queue a reply for one link, whether or not it has finished connecting.
pub fn reply(source: Source, text: &str) {
    match source {
        Source::Usb => USB.reply(text),
        Source::Ble => BLE.reply(text),
        Source::Relay => RELAY.reply(text),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Key(badge_ui::Key),
    Screen,
    Bootsel,
    Demo,
    Ambient,
    Help,
}

pub const HELP: &str = "commands: mirror [on|off] | screen | demo | ambient | key up|down|left|right|select | bootsel (USB only)\r\n";

/// Bytes for one host, plus whether it is listening and wants mirror frames.
pub struct Outbox<const N: usize> {
    pipe: Pipe<CriticalSectionRawMutex, N>,
    connected: AtomicBool,
    mirror: AtomicBool,
    source: Source,
}

impl<const N: usize> Outbox<N> {
    const fn new(source: Source) -> Self {
        Self {
            pipe: Pipe::new(),
            // USB has no handshake, so it is always listening.
            connected: AtomicBool::new(matches!(source, Source::Usb)),
            mirror: AtomicBool::new(false),
            source,
        }
    }

    pub fn connect(&self) {
        self.pipe.clear();
        self.connected.store(true, Ordering::Relaxed);
    }

    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.mirror.store(false, Ordering::Relaxed);
        self.pipe.clear();
    }

    /// Queue bytes whole or not at all, so frames never tear; false if they didn't fit.
    fn write(&self, bytes: &[u8]) -> bool {
        if !self.connected.load(Ordering::Relaxed) || self.pipe.free_capacity() < bytes.len() {
            return false;
        }
        self.pipe.try_write(bytes).is_ok()
    }

    /// Queue a reply, even before the host is connected (e.g. during the Bluetooth handshake).
    /// Replies matter more than queued logs: a USB host that just opened the port may find the
    /// buffer full of output nobody read, so make room by dropping that.
    pub fn reply(&self, text: &str) {
        if self.pipe.free_capacity() < text.len() {
            self.pipe.clear();
        }
        let _ = self.pipe.try_write(text.as_bytes());
    }

    /// Wait for queued bytes for the transport to send.
    pub async fn read(&self, buf: &mut [u8]) -> usize {
        self.pipe.read(buf).await
    }

    /// Act on one line from the host.
    pub async fn command(&self, line: &str) {
        if line.starts_with("ota ") {
            // Only over USB: writing flash means pausing core 1, which runs the radio, and that
            // hangs while Bluetooth or the relay is carrying the update.
            if self.source != Source::Usb {
                self.reply("ota error updates only work over USB\r\n");
                return;
            }
            if let Ok(line) = heapless::String::try_from(line) {
                OTA.send((self.source, line)).await;
            }
            return;
        }
        match parse(line) {
            Line::Mirror(on) => self.mirror.store(on, Ordering::Relaxed),
            // Only a cable can reboot the badge into BOOTSEL, since getting out again needs one.
            Line::Command(Command::Bootsel) if self.source != Source::Usb => {
                self.write(b"bootsel only works over USB\r\n");
            }
            Line::Command(command) => {
                let _ = COMMANDS.try_send(command);
            }
        }
    }

    fn mirroring(&self) -> bool {
        self.mirror.load(Ordering::Relaxed)
    }
}

/// Send text (logs, snapshots) to every connected host without blocking.
pub fn write(text: &str) {
    USB.write(text.as_bytes());
    BLE.write(text.as_bytes());
    RELAY.write(text.as_bytes());
}

/// Whether any host wants mirror frames.
pub fn mirroring() -> bool {
    USB.mirroring() || BLE.mirroring() || RELAY.mirroring()
}

/// Send a mirror frame to the hosts that asked for one. A host that hasn't caught up with the
/// last frame skips this one.
pub fn write_frame(frame: &str) {
    let bytes = frame.as_bytes();
    if USB.mirroring() {
        USB.write(bytes);
    }
    if BLE.mirroring() {
        BLE.write(bytes);
    }
    if RELAY.mirroring() {
        RELAY.write(bytes);
    }
}

enum Line {
    Mirror(bool),
    Command(Command),
}

fn parse(line: &str) -> Line {
    use badge_ui::Key;
    let mut words = line.split_whitespace();
    Line::Command(match (words.next(), words.next()) {
        (Some("mirror"), Some("off")) => return Line::Mirror(false),
        (Some("mirror"), _) => return Line::Mirror(true),
        (Some("screen"), _) => Command::Screen,
        (Some("bootsel"), _) => Command::Bootsel,
        (Some("demo"), _) => Command::Demo,
        (Some("ambient"), _) => Command::Ambient,
        (Some("key"), Some("up")) => Command::Key(Key::Up),
        (Some("key"), Some("down")) => Command::Key(Key::Down),
        (Some("key"), Some("left")) => Command::Key(Key::Left),
        (Some("key"), Some("right")) => Command::Key(Key::Right),
        (Some("key"), Some("select")) => Command::Key(Key::Select),
        _ => Command::Help,
    })
}

/// Accumulates bytes into newline-terminated lines without allocating.
pub struct LineBuffer {
    buf: [u8; LINE],
    len: usize,
    ready: bool,
}

impl LineBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0; LINE],
            len: 0,
            ready: false,
        }
    }

    pub fn push(&mut self, b: u8) -> Option<&str> {
        if self.ready {
            self.len = 0;
            self.ready = false;
        }
        match b {
            b'\r' | b'\n' => {
                self.ready = true;
                core::str::from_utf8(&self.buf[..self.len])
                    .ok()
                    .filter(|s| !s.is_empty())
            }
            _ if self.len < self.buf.len() => {
                self.buf[self.len] = b;
                self.len += 1;
                None
            }
            _ => None,
        }
    }
}
