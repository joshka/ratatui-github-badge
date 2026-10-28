//! Firmware updates over USB, into the other half of an A/B partition table (`partitions.json`).
//!
//! Only over USB: writing flash means pausing core 1, and that pause hangs while core 1 is busy
//! carrying the update over Bluetooth or the relay (`link.rs` refuses those).
//!
//! The boot ROM maps whichever partition booted to 0x10000000, so one build runs from either.
//! An update is written to the other partition, then booted on trial ("try before you buy"):
//! the new firmware confirms itself after running for [`CONFIRM_AFTER`]. If it stalls or crashes
//! first, the watchdog resets and the ROM boots the old firmware again.
//!
//! The host (`cargo badge flash`) drives it with text lines, sector by sector, waiting for each
//! `ota ok`:
//!
//! ```text
//! host:  ota status                       badge: ota: running partition A (etc.)
//! host:  ota begin <size>
//! badge: ota ready <sectors>
//! host:  ota hashes <first>                 badge: ota hashes <first> <hash>… (8 sectors)
//! host:  ota copy <sector>                  sector unchanged: copy it from the running firmware
//!        ota data <base64>… ota write <sector> <hash>     or send it
//! badge: ota ok <sector> | ota bad <sector> <reason>
//! host:  ota finish
//! badge: ota rebooting
//! ```
//!
//! Hashes are the first 8 bytes of a sector's SHA-256, as hex, over the full 4 KB with the
//! image padded with 0xFF.
use alloc::{format, string::String};
use core::fmt::Write;

use embassy_rp::{
    Peri,
    flash::{Blocking, Flash},
    peripherals::FLASH,
};
use embassy_time::{Duration, Instant};
use sha2::{Digest, Sha256};

use crate::link::Source;

pub const SECTOR: usize = 4096;
/// The partitions from `partitions.json`, as flash offsets.
const PARTITIONS: [u32; 2] = [0x2000, 0x16_2000];
const PARTITION_SIZE: u32 = 0x16_0000;
const FLASH_SIZE: usize = 16 * 1024 * 1024;
/// The XIP alias that bypasses the ROM's address translation, for reading the other partition.
const UNTRANSLATED: u32 = 0x1c00_0000;
/// How long new firmware must run before it confirms itself.
const CONFIRM_AFTER: Duration = Duration::from_secs(10);

pub struct Ota {
    flash: Flash<'static, FLASH, Blocking, FLASH_SIZE>,
    /// The partition that booted, if any.
    booted: Option<usize>,
    pending_since: Option<Instant>,
    update: Option<Update>,
}

struct Update {
    source: Source,
    target: u32,
    sectors: usize,
    buf: alloc::vec::Vec<u8>,
}

impl Ota {
    pub fn new(flash: Peri<'static, FLASH>) -> Self {
        let boot = boot_info();
        let booted = match boot.partition {
            p @ 0..=1 => Some(p as usize),
            _ => None,
        };
        let buy_pending = boot.tbyb & 0x1 != 0;
        Self {
            flash: Flash::new_blocking(flash),
            booted,
            pending_since: buy_pending.then(Instant::now),
            update: None,
        }
    }

    /// A line for the log at boot, describing what booted.
    pub fn describe(&self) -> String {
        self.describe_partition()
    }

    fn describe_partition(&self) -> String {
        match (self.booted, self.pending_since) {
            (None, _) => "ota: no partition table; flash partitions.uf2 to enable updates".into(),
            (Some(p), Some(_)) => format!(
                "ota: trying new firmware in partition {}; confirming in {} s",
                name(p),
                CONFIRM_AFTER.as_secs()
            ),
            (Some(p), None) => format!("ota: running partition {}", name(p)),
        }
    }

    /// Call every frame: confirms new firmware once it has run long enough.
    pub fn poll(&mut self, watchdog: &mut embassy_rp::watchdog::Watchdog) {
        let Some(since) = self.pending_since else { return };
        if since.elapsed() < CONFIRM_AFTER {
            return;
        }
        self.pending_since = None;
        crate::link::write("ota: confirming, then rebooting\r\n");
        watchdog.feed(Duration::from_secs(5));
        // A word-aligned buffer: the ROM runs with flash off, so a fault in there can't reach our
        // fault handler, and the core would lock up until the watchdog resets it.
        let mut workarea = alloc::vec![0u32; SECTOR / 4];
        crate::recovery::clear_alive();
        // The ROM rewrites flash, which stops instruction fetches, so core 1 (the radio) has to
        // stop. Asking it to pause over the FIFO hung in trial boots, so power it off instead and
        // reboot afterwards to bring the radio back. Take the critical section first: core 1
        // could otherwise be powered off while holding its spinlock, deadlocking everything.
        // Never leave it: the reboot comes first.
        critical_section::with(|_| {
            embassy_rp::pac::PSM.frce_off().modify(|w| w.set_proc1(true));
            while !embassy_rp::pac::PSM.frce_off().read().proc1() {}
            // Nothing can report a failure from here; if it fails, the reboot lands on the old
            // firmware, as a failed trial should.
            unsafe { embassy_rp::rom_data::explicit_buy(workarea.as_mut_ptr().cast(), SECTOR as u32) };
            // A normal reboot: the ROM boots the newer of the two images, which is now this one.
            embassy_rp::rom_data::reboot(0x0100, 100, 0, 0);
        });
    }

    /// Handle one `ota …` line from the host.
    pub fn command(&mut self, source: Source, line: &str) {
        let mut words = line.split_whitespace().skip(1);
        let reply = match (words.next(), words.next(), words.next()) {
            (Some("begin"), Some(size), _) => self.begin(source, size),
            (Some("hashes"), Some(first), _) => self.hashes(source, first),
            (Some("copy"), Some(sector), _) => self.copy(source, sector),
            (Some("data"), Some(data), _) => self.data(source, data),
            (Some("write"), Some(sector), Some(hash)) => self.write(source, sector, hash),
            (Some("finish"), _, _) => self.finish(source),
            (Some("status"), _, _) => Ok(Some(format!("{}\r\n", self.describe()))),
            _ => Err("unknown ota command".into()),
        };
        match reply {
            Ok(Some(text)) => crate::link::reply(source, &text),
            Ok(None) => {}
            Err(error) => crate::link::reply(source, &format!("ota error {error}\r\n")),
        }
    }

    fn begin(&mut self, source: Source, size: &str) -> Result<Option<String>, String> {
        let booted = self.booted.ok_or("no partition table")?;
        let size: usize = size.parse().map_err(|_| "bad size")?;
        if size == 0 || size > PARTITION_SIZE as usize {
            return Err(format!("image is {size} bytes; partitions hold {PARTITION_SIZE}"));
        }
        let sectors = size.div_ceil(SECTOR);
        self.update = Some(Update {
            source,
            target: PARTITIONS[1 - booted],
            sectors,
            buf: alloc::vec::Vec::with_capacity(SECTOR),
        });
        Ok(Some(format!("ota ready {sectors}\r\n")))
    }

    /// Hashes of eight sectors of the running firmware, so the host can skip what's unchanged.
    /// One line per request, so a reply never outgrows a small outbox.
    fn hashes(&mut self, source: Source, first: &str) -> Result<Option<String>, String> {
        let first = self.sector(source, first)?;
        let sectors = self.update(source)?.sectors;
        let mut line = format!("ota hashes {first}");
        for sector in first..(first + 8).min(sectors) {
            let _ = write!(line, " {}", Hex(&hash(running(sector))));
        }
        line.push_str("\r\n");
        Ok(Some(line))
    }

    fn update(&mut self, source: Source) -> Result<&mut Update, String> {
        match &mut self.update {
            Some(update) if update.source == source => Ok(update),
            _ => Err("no update in progress; send ota begin".into()),
        }
    }

    fn copy(&mut self, source: Source, sector: &str) -> Result<Option<String>, String> {
        let sector = self.sector(source, sector)?;
        let data = running(sector).to_vec();
        self.program(sector, &data)
    }

    fn data(&mut self, source: Source, data: &str) -> Result<Option<String>, String> {
        let update = self.update(source)?;
        let had = update.buf.len();
        if !base64_decode(data, &mut update.buf) || update.buf.len() > SECTOR {
            let got = update.buf.len();
            update.buf.clear();
            let at = data.bytes().position(|c| !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=')).unwrap_or(0);
            return Err(format!(
                "bad data: {} chars, buffer {had} -> {got}, bad at {at}: {:?}",
                data.len(),
                &data.as_bytes()[at.saturating_sub(24)..(at + 24).min(data.len())]
            ));
        }
        Ok(None)
    }

    fn write(&mut self, source: Source, sector: &str, expected: &str) -> Result<Option<String>, String> {
        let sector = self.sector(source, sector)?;
        let update = self.update(source)?;
        let mut data = core::mem::take(&mut update.buf);
        data.resize(SECTOR, 0xFF);
        if format!("{}", Hex(&hash(&data))) != expected {
            return Ok(Some(format!("ota bad {sector} hash mismatch\r\n")));
        }
        let reply = self.program(sector, &data);
        if let Some(update) = &mut self.update {
            data.clear();
            update.buf = data;
        }
        reply
    }

    fn sector(&mut self, source: Source, sector: &str) -> Result<usize, String> {
        let sectors = self.update(source)?.sectors;
        match sector.parse() {
            Ok(sector) if sector < sectors => Ok(sector),
            _ => Err(format!("bad sector {sector}")),
        }
    }

    /// Erase, write and read back one sector of the target partition.
    fn program(&mut self, sector: usize, data: &[u8]) -> Result<Option<String>, String> {
        let target = self.update.as_ref().ok_or("no update")?.target;
        let offset = target + (sector * SECTOR) as u32;
        self.flash.blocking_erase(offset, offset + SECTOR as u32).map_err(|e| format!("erase: {e:?}"))?;
        self.flash.blocking_write(offset, data).map_err(|e| format!("write: {e:?}"))?;
        if untranslated(offset) != data {
            return Ok(Some(format!("ota bad {sector} read back differently\r\n")));
        }
        Ok(Some(format!("ota ok {sector}\r\n")))
    }

    fn finish(&mut self, source: Source) -> Result<Option<String>, String> {
        let target = self.update(source)?.target;
        self.update = None;
        crate::link::reply(source, "ota rebooting\r\n");
        crate::recovery::clear_alive();
        // FLASH_UPDATE: boot the new partition on trial. The delay lets the reply go out.
        embassy_rp::rom_data::reboot(0x0004 | 0x0100, 500, 0x1000_0000 + target, 0);
        Ok(None)
    }
}

fn name(partition: usize) -> char {
    (b'A' + partition as u8) as char
}

/// One sector of the running firmware, through the ROM's translation.
fn running(sector: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts((0x1000_0000 + sector * SECTOR) as *const u8, SECTOR) }
}

/// One sector at a raw flash offset, bypassing translation (and the cache, which may be stale).
fn untranslated(offset: u32) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts((UNTRANSLATED + offset) as *const u8, SECTOR) }
}

fn hash(data: &[u8]) -> [u8; 8] {
    Sha256::digest(data)[..8].try_into().unwrap()
}

struct BootInfo {
    partition: i8,
    tbyb: u8,
}

/// The ROM's record of this boot (`get_sys_info` with BOOT_INFO).
fn boot_info() -> BootInfo {
    let mut words = [0u32; 5];
    let n = unsafe { embassy_rp::rom_data::get_sys_info(words.as_mut_ptr(), words.len(), 0x0040) };
    if n < 2 {
        return BootInfo { partition: -1, tbyb: 0 };
    }
    let [_, _, partition, tbyb] = words[1].to_le_bytes();
    BootInfo { partition: partition as i8, tbyb }
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// Standard base64 with padding, appended to `out`. False if `text` isn't valid.
fn base64_decode(text: &str, out: &mut alloc::vec::Vec<u8>) -> bool {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return false;
    }
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            let Some(v) = value(c) else { return false };
            n = n << 6 | v as u32;
        }
        n <<= 6 * pad as u32;
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    true
}
