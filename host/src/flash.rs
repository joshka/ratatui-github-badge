//! `cargo badge flash`: update the firmware over whichever link is open (see `src/ota.rs` in the
//! firmware for the protocol). Sectors the badge already has are copied on the badge, so only
//! what changed crosses the link.
use std::{
    io::Write,
    path::Path,
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, eyre},
};
use object::read::elf::{ElfFile32, ProgramHeader};
use sha2::{Digest, Sha256};

use crate::{Incoming, link::Link, reader, send};

const SECTOR: usize = 4096;
/// Bytes per `ota data` line; 192 is 256 characters of base64.
const CHUNK: usize = 192;
const FLASH: std::ops::Range<u64> = 0x1000_0000..0x1100_0000;
/// Generous for Bluetooth, where a sector's data takes seconds to send before its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const RETRIES: usize = 3;

pub fn flash(link: Link, elf: &Path) -> Result<()> {
    let image = image(elf)?;
    let sectors = image.len().div_ceil(SECTOR);
    let Link { mut writer, reader: port, name } = link;
    let events = reader(port);
    let mut out = std::io::stdout();
    writeln!(out, "flashing {} ({} KB, {sectors} sectors) over {name}", elf.display(), image.len() / 1024)?;

    // Retry once: a reply can be lost to output queued before we connected.
    let begin = format!("ota begin {}", image.len());
    send(writer.as_mut(), &begin)?;
    let ready = match wait(&events, |line| line.strip_prefix("ota ready ").map(str::to_string)) {
        Ok(ready) => ready,
        Err(_) => {
            send(writer.as_mut(), &begin)?;
            wait(&events, |line| line.strip_prefix("ota ready ").map(str::to_string)).wrap_err("waiting for ota ready")?
        }
    };
    if ready.parse::<usize>()? != sectors {
        bail!("the badge expects {ready} sectors, not {sectors}");
    }
    let mut theirs = vec![None; sectors];
    for first in (0..sectors).step_by(8) {
        send(writer.as_mut(), &format!("ota hashes {first}"))?;
        let line = wait(&events, |line| line.strip_prefix("ota hashes ").map(str::to_string))
            .wrap_err_with(|| format!("waiting for the hashes from sector {first}"))?;
        let mut words = line.split_whitespace();
        let first: usize = words.next().ok_or_else(|| eyre!("bad hashes line"))?.parse()?;
        for (i, hash) in words.enumerate() {
            if let Some(slot) = theirs.get_mut(first + i) {
                *slot = Some(hash.to_string());
            }
        }
    }

    let start = Instant::now();
    let (mut copied, mut sent) = (0, 0);
    for sector in 0..sectors {
        let mut data = image[sector * SECTOR..((sector + 1) * SECTOR).min(image.len())].to_vec();
        data.resize(SECTOR, 0xFF);
        let ours = hex(&Sha256::digest(&data)[..8]);
        let unchanged = theirs[sector].as_deref() == Some(ours.as_str());
        let mut attempt = 0;
        loop {
            if unchanged && attempt == 0 {
                send(writer.as_mut(), &format!("ota copy {sector}"))?;
            } else {
                for chunk in data.chunks(CHUNK) {
                    send(writer.as_mut(), &format!("ota data {}", STANDARD.encode(chunk)))?;
                }
                send(writer.as_mut(), &format!("ota write {sector} {ours}"))?;
            }
            let reply = wait(&events, |line| {
                let rest = line.strip_prefix("ota ")?;
                (rest.starts_with("ok ") || rest.starts_with("bad ")).then(|| rest.to_string())
            })
            .wrap_err_with(|| format!("waiting for sector {sector} to be written"))?;
            if reply == format!("ok {sector}") {
                break;
            }
            attempt += 1;
            if attempt > RETRIES {
                bail!("sector {sector} failed: {reply}");
            }
        }
        if unchanged && attempt == 0 { copied += 1 } else { sent += 1 }
        write!(out, "\r{}/{sectors} sectors, {sent} sent, {copied} copied", sector + 1)?;
        out.flush()?;
    }
    writeln!(out, " in {:.1} s", start.elapsed().as_secs_f32())?;

    send(writer.as_mut(), "ota finish")?;
    wait(&events, |line| (line == "ota rebooting").then_some(()))?;
    writeln!(out, "rebooting into the new firmware; it confirms itself after 10 s of running")?;
    Ok(())
}

/// Wait for a reply `matches` accepts, failing on an `ota error`, a timeout or a disconnect.
fn wait<T>(events: &Receiver<Incoming>, matches: impl Fn(&str) -> Option<T>) -> Result<T> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(Incoming::Line(line)) => {
                if let Some(error) = line.strip_prefix("ota error ") {
                    bail!("the badge refused: {error}");
                }
                if let Some(value) = matches(&line) {
                    return Ok(value);
                }
            }
            Ok(Incoming::Frame(_)) => {}
            Ok(Incoming::Closed(error)) => bail!("badge disconnected: {error}"),
            Err(RecvTimeoutError::Timeout) => bail!("the badge didn't answer in {}s", REPLY_TIMEOUT.as_secs()),
            Err(RecvTimeoutError::Disconnected) => bail!("badge disconnected"),
        }
    }
}

/// The ELF's flash contents as one image from 0x10000000, gaps filled with 0xFF.
fn image(elf: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(elf).map_err(|e| eyre!("reading {}: {e}", elf.display()))?;
    let file = ElfFile32::<object::Endianness>::parse(&*data)?;
    let endian = file.endian();
    let mut image = Vec::new();
    // Load (physical) addresses: initialised RAM data is stored in flash after the code.
    for header in file.elf_program_headers() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let load = u64::from(header.p_paddr(endian));
        let bytes = header.data(endian, &*data).map_err(|()| eyre!("bad program header"))?;
        if bytes.is_empty() || !FLASH.contains(&load) {
            continue;
        }
        let offset = (load - FLASH.start) as usize;
        if image.len() < offset + bytes.len() {
            image.resize(offset + bytes.len(), 0xFF);
        }
        image[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
    if image.is_empty() {
        bail!("{} has nothing to put in flash", elf.display());
    }
    Ok(image)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
