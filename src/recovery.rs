//! Preserve a panic report across a warm reset without allocating in the panic handler.
use core::{
    cell::RefCell,
    fmt::{self, Write},
    mem::MaybeUninit,
    panic::PanicInfo,
};
use critical_section::Mutex;

const PANIC_MAGIC: u32 = 0x52415450;
const REPORT_SIZE: usize = 256;

#[derive(Clone, Copy)]
pub struct Report {
    pub text: [u8; REPORT_SIZE],
    pub len: usize,
}

impl Report {
    const fn new() -> Self {
        Self {
            text: [0; REPORT_SIZE],
            len: 0,
        }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.text[..self.len.min(REPORT_SIZE)])
            .unwrap_or("Panic report contains non-UTF8 text")
    }
}

impl Write for Report {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut length = text.len().min(REPORT_SIZE - self.len);
        while !text.is_char_boundary(length) {
            length -= 1;
        }
        self.text[self.len..self.len + length].copy_from_slice(&text.as_bytes()[..length]);
        self.len += length;
        Ok(())
    }
}

#[unsafe(link_section = ".uninit.panic_report")]
static mut PANIC_REPORT: MaybeUninit<Report> = MaybeUninit::uninit();
static LAST_REPORT: Mutex<RefCell<Option<Report>>> = Mutex::new(RefCell::new(None));

pub fn init() -> bool {
    let watchdog = embassy_rp::pac::WATCHDOG;
    let report = if watchdog.scratch0().read() == PANIC_MAGIC {
        // Only our panic handler sets this magic, after writing a complete Report.
        Some(unsafe { core::ptr::read_volatile((&raw const PANIC_REPORT).cast::<Report>()) })
    } else if watchdog.reason().read().timer() {
        let mut report = Report::new();
        let _ = report.write_str(
            "Watchdog recovered a stalled task. Wi-Fi remains off until explicitly started.",
        );
        Some(report)
    } else {
        None
    };
    watchdog.scratch0().write_value(0);
    critical_section::with(|cs| *LAST_REPORT.borrow(cs).borrow_mut() = report);
    report.is_some()
}

pub fn report() -> Option<Report> {
    critical_section::with(|cs| *LAST_REPORT.borrow(cs).borrow())
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    cortex_m::interrupt::disable();
    let mut report = Report::new();
    if let Some(location) = info.location() {
        // Keep the useful filename and line rather than a long registry/home path.
        let file = location.file().rsplit('/').next().unwrap_or("unknown");
        let _ = write!(report, "{file}:{}: {}", location.line(), info.message());
    } else {
        let _ = write!(report, "{}", info.message());
    }
    // .uninit is retained by cortex-m-rt during warm reset; publish only after writing.
    unsafe {
        core::ptr::write_volatile((&raw mut PANIC_REPORT).cast::<Report>(), report);
    }
    embassy_rp::pac::WATCHDOG
        .scratch0()
        .write_value(PANIC_MAGIC);
    cortex_m::peripheral::SCB::sys_reset()
}
