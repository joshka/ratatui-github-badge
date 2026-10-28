//! USB: a CDC serial port for logs, screen mirroring and remote keys, plus the Pico SDK
//! "reset interface" so `picotool load -f` can reboot the badge into BOOTSEL by itself.
use embassy_executor::SendSpawner;
use embassy_rp::{
    bind_interrupts,
    peripherals::USB,
    usb::{Driver, InterruptHandler},
};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::Channel,
    pipe::Pipe,
};
use embassy_usb::{
    Builder, Handler,
    class::cdc_acm::{CdcAcmClass, Receiver, Sender, State},
    control::{OutResponse, Recipient, Request, RequestType},
    types::InterfaceNumber,
};
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::join::join3;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

type UsbDriver = Driver<'static, USB>;

/// Bytes for the host. Writers drop output when it is full (e.g. nobody listening).
static TX: Pipe<CriticalSectionRawMutex, 32768> = Pipe::new();
/// Commands from the host.
pub static COMMANDS: Channel<CriticalSectionRawMutex, Command, 8> = Channel::new();
/// Set by picotool's reset request; the main loop reboots (it owns the watchdog, which would
/// otherwise cancel a delayed ROM reboot).
pub static REBOOT_TO_BOOTSEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Key(badge_ui::Key),
    Mirror,
    Screen,
    Bootsel,
    Demo,
    Help,
}

/// Queue text for the host without blocking; returns false if it didn't fit.
pub fn write(text: &str) -> bool {
    if TX.free_capacity() < text.len() {
        return false;
    }
    TX.try_write(text.as_bytes()).is_ok()
}

/// Runs on a high-priority interrupt executor so enumeration and transfers stay responsive
/// while the main loop is busy drawing (a frame blocks for tens of milliseconds).
pub fn start(spawner: SendSpawner, usb: embassy_rp::Peri<'static, USB>) {
    spawner.spawn(usb_main(usb).unwrap());
}

/// Everything USB is built inside this one task, since the USB types aren't `Send`.
#[embassy_executor::task]
async fn usb_main(usb: embassy_rp::Peri<'static, USB>) {
    let driver = Driver::new(usb, Irqs);
    // Raspberry Pi's Pico SDK stdio-USB IDs: picotool only looks for its reset interface on these.
    let mut config = embassy_usb::Config::new(0x2E8A, 0x0009);
    config.manufacturer = Some("ratatui");
    config.product = Some("Ratatui Universe badge");
    // The boot ROM reports the chip ID as its serial; matching it lets `picotool -f` follow
    // the badge across the reboot into BOOTSEL.
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();
    let serial = SERIAL.init(*b"0000000000000000");
    let id = embassy_rp::otp::get_chipid().unwrap_or(0);
    for (i, byte) in serial.iter_mut().enumerate() {
        *byte = b"0123456789ABCDEF"[(id >> (60 - 4 * i) & 0xF) as usize];
    }
    config.serial_number = Some(core::str::from_utf8(serial).unwrap());
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State> = StaticCell::new();
    static RESET: StaticCell<ResetInterface> = StaticCell::new();
    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), 64);
    let reset = RESET.init(ResetInterface { interface: InterfaceNumber(0) });
    {
        let mut function = builder.function(0xFF, 0x00, 0x01);
        let mut interface = function.interface();
        reset.interface = interface.interface_number();
        interface.alt_setting(0xFF, 0x00, 0x01, None);
    }
    builder.handler(reset);
    let mut device = builder.build();
    let (sender, receiver) = class.split();
    join3(device.run(), transmit(sender), receive(receiver)).await;
}

async fn transmit(mut sender: Sender<'static, UsbDriver>) {
    let mut buf = [0u8; 64];
    loop {
        sender.wait_connection().await;
        loop {
            let n = TX.read(&mut buf).await;
            if sender.write_packet(&buf[..n]).await.is_err() {
                break;
            }
        }
    }
}

async fn receive(mut receiver: Receiver<'static, UsbDriver>) {
    let mut packet = [0u8; 64];
    let mut line = heapless_line::Line::new();
    loop {
        receiver.wait_connection().await;
        while let Ok(n) = receiver.read_packet(&mut packet).await {
            for &b in &packet[..n] {
                if let Some(text) = line.push(b)
                    && let Some(command) = parse(text)
                {
                    let _ = COMMANDS.try_send(command);
                }
            }
        }
    }
}

fn parse(line: &str) -> Option<Command> {
    use badge_ui::Key;
    let mut words = line.split_whitespace();
    Some(match (words.next()?, words.next()) {
        ("mirror", _) => Command::Mirror,
        ("screen", _) => Command::Screen,
        ("bootsel", _) => Command::Bootsel,
        ("demo", _) => Command::Demo,
        ("key", Some("up")) => Command::Key(Key::Up),
        ("key", Some("down")) => Command::Key(Key::Down),
        ("key", Some("left")) => Command::Key(Key::Left),
        ("key", Some("right")) => Command::Key(Key::Right),
        ("key", Some("select")) => Command::Key(Key::Select),
        _ => Command::Help,
    })
}

/// Reboot into BOOTSEL (or back to the app) when picotool asks, as the Pico SDK does.
struct ResetInterface {
    interface: InterfaceNumber,
}

impl Handler for ResetInterface {
    fn control_out(&mut self, req: Request, _data: &[u8]) -> Option<OutResponse> {
        let ours = matches!(req.request_type, RequestType::Class | RequestType::Vendor)
            && req.recipient == Recipient::Interface
            && req.index == u16::from(self.interface.0);
        if !ours {
            return None;
        }
        match req.request {
            // RESET_REQUEST_BOOTSEL
            0x01 => REBOOT_TO_BOOTSEL.store(true, Ordering::Relaxed),
            // RESET_REQUEST_FLASH: a plain reboot isn't needed by picotool here.
            0x02 => {}
            _ => return Some(OutResponse::Rejected),
        }
        Some(OutResponse::Accepted)
    }
}

/// Reboot now; `kind` 0x0002 is BOOTSEL. NO_RETURN_ON_SUCCESS so nothing feeds the watchdog after.
pub fn reboot(kind: u32) {
    crate::recovery::clear_alive();
    embassy_rp::rom_data::reboot(kind | 0x0100, 10, 0, 0);
}

pub fn reboot_requested() -> bool {
    REBOOT_TO_BOOTSEL.load(Ordering::Relaxed)
}

mod heapless_line {
    /// Accumulates bytes into newline-terminated lines without allocating.
    pub struct Line {
        buf: [u8; 64],
        len: usize,
        ready: bool,
    }

    impl Line {
        pub const fn new() -> Self {
            Self { buf: [0; 64], len: 0, ready: false }
        }

        pub fn push(&mut self, b: u8) -> Option<&str> {
            if self.ready {
                self.len = 0;
                self.ready = false;
            }
            match b {
                b'\r' | b'\n' => {
                    self.ready = true;
                    core::str::from_utf8(&self.buf[..self.len]).ok().filter(|s| !s.is_empty())
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
}
