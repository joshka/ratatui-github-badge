//! Bluetooth LE: the badge's Nordic UART Service. Notifications carry its output and writes
//! carry command lines. btleplug is async, so it runs on its own thread with a Tokio runtime.
//!
//! With `ble.key` in badge.toml, the host and badge prove to each other that they know it
//! (see `src/radio/auth.rs` in the firmware), so a badge with the same name but a different key
//! is skipped.
use std::{
    collections::HashSet,
    io,
    pin::Pin,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use btleplug::{
    api::{Central, Manager as _, Peripheral as _, ScanFilter, ValueNotification, WriteType},
    platform::{Manager, Peripheral, PeripheralId},
};
use color_eyre::{
    Result,
    eyre::{OptionExt, bail, eyre},
};
use futures::{Stream, StreamExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::{Uuid, uuid};

use super::{ChannelReader, ChannelWriter, Link};

const SERVICE: Uuid = uuid!("6e400001-b5a3-f393-e0a9-e50e24dcca9e");
const RX: Uuid = uuid!("6e400002-b5a3-f393-e0a9-e50e24dcca9e");
const TX: Uuid = uuid!("6e400003-b5a3-f393-e0a9-e50e24dcca9e");
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
/// A badge that macOS still lists but that isn't advertising can make `connect` wait forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often to ask whether the link is still up: notifications just stop when it drops.
const LIVENESS: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// Bytes per write. Each write waits for the badge to acknowledge it, so small writes are slow
/// (an update at 20 bytes would take most of half an hour). macOS negotiates a bigger packet
/// size with the badge, and splits longer writes itself if needed.
const WRITE_CHUNK: usize = 180;

/// Connect to a badge called `name` that knows `key`.
pub fn connect(name: &str, key: Option<String>) -> Result<Link> {
    let (data_tx, data_rx) = mpsc::channel();
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let name = name.to_string();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let peripheral = match open(&name, key.as_deref()).await {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(name));
            let error = pump(&peripheral, &data_tx, command_rx).await.err();
            let _ = peripheral.disconnect().await;
            let reason = error.map_or_else(|| "disconnected".to_string(), |e| e.to_string());
            let _ = data_tx.send(Err(io::Error::new(io::ErrorKind::BrokenPipe, reason)));
            let _ = closed_tx.send(());
        });
    });
    let name = ready_rx.recv()??;
    Ok(Link {
        reader: Box::new(ChannelReader::new(data_rx)),
        writer: Box::new(ChannelWriter(move |bytes| command_tx.send(bytes).is_ok())),
        name: format!("{name} (BLE)"),
        closed: Some(closed_rx),
    })
}

/// Scan for badges called `name`, and connect to the first that passes the handshake.
async fn open(name: &str, key: Option<&str>) -> Result<Peripheral> {
    let adapter = Manager::new()
        .await?
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_eyre("no Bluetooth adapter")?;
    adapter.start_scan(ScanFilter { services: vec![SERVICE] }).await?;
    let deadline = Instant::now() + SCAN_TIMEOUT;
    let mut tried: HashSet<PeripheralId> = HashSet::new();
    let mut last_error = None;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for peripheral in adapter.peripherals().await? {
            let Some(properties) = peripheral.properties().await? else { continue };
            if !properties.services.contains(&SERVICE)
                || properties.local_name.as_deref() != Some(name)
                || !tried.insert(peripheral.id())
            {
                continue;
            }
            match try_badge(&peripheral, key).await {
                Ok(()) => {
                    adapter.stop_scan().await?;
                    return Ok(peripheral);
                }
                Err(error) => {
                    let _ = peripheral.disconnect().await;
                    last_error = Some(error);
                }
            }
        }
    }
    let _ = adapter.stop_scan().await;
    match last_error {
        Some(error) => Err(error),
        None => bail!("no badge called {name:?} found over Bluetooth in {}s", SCAN_TIMEOUT.as_secs()),
    }
}

async fn try_badge(peripheral: &Peripheral, key: Option<&str>) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        peripheral.connect().await?;
        peripheral.discover_services().await
    })
    .await
    .map_err(|_| eyre!("the badge didn't accept a connection in {}s", CONNECT_TIMEOUT.as_secs()))??;
    let tx = characteristic(peripheral, TX)?;
    peripheral.subscribe(&tx).await?;
    if let Some(key) = key {
        // Retry: a stray line can still upset one attempt, and each uses fresh nonces.
        let mut result = Ok(());
        for _ in 0..3 {
            result = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(peripheral, key.as_bytes()))
                .await
                .map_err(|_| eyre!("the badge didn't answer the handshake: was it built without ble.key?"))
                .and_then(|r| r);
            if result.is_ok() {
                break;
            }
        }
        result?;
    }
    Ok(())
}

/// Prove we know the key, and check the badge does too. See `src/radio/auth.rs`.
async fn handshake(peripheral: &Peripheral, key: &[u8]) -> Result<()> {
    let rx = characteristic(peripheral, RX)?;
    let mut notifications = peripheral.notifications().await?;
    let mut host_nonce = [0; 16];
    getrandom::fill(&mut host_nonce).map_err(|e| eyre!("no randomness: {e}"))?;
    write_line(peripheral, &rx, &format!("hello {}", hex(&host_nonce))).await?;

    // Skip anything else the badge sends first (late output from an earlier session).
    let line = loop {
        let line = read_line(&mut notifications).await?;
        if line.starts_with("challenge ") {
            break line;
        }
    };
    let mut words = line.split_whitespace();
    let (Some("challenge"), Some(badge_nonce), Some(proof)) = (words.next(), words.next(), words.next()) else {
        bail!("unexpected handshake reply {line:?}");
    };
    let badge_nonce = unhex(badge_nonce)?;
    mac(key, b"badge", &host_nonce, &badge_nonce)
        .verify_slice(&unhex(proof)?)
        .map_err(|_| eyre!("a badge with this name has a different ble.key"))?;

    let ours = mac(key, b"host", &badge_nonce, &host_nonce).finalize().into_bytes();
    write_line(peripheral, &rx, &format!("auth {}", hex(&ours))).await?;
    let reply = loop {
        let line = read_line(&mut notifications).await?;
        if line.starts_with("auth ") {
            break line;
        }
    };
    match reply.as_str() {
        "auth ok" => Ok(()),
        other => bail!("the badge rejected our key: {other:?}"),
    }
}

fn mac(key: &[u8], role: &[u8], first: &[u8], second: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(role);
    mac.update(first);
    mac.update(second);
    mac
}

async fn write_line(peripheral: &Peripheral, rx: &btleplug::api::Characteristic, line: &str) -> Result<()> {
    for chunk in format!("{line}\n").as_bytes().chunks(WRITE_CHUNK) {
        peripheral.write(rx, chunk, WriteType::WithResponse).await?;
    }
    Ok(())
}

async fn read_line(notifications: &mut Pin<Box<dyn Stream<Item = ValueNotification> + Send>>) -> Result<String> {
    let mut line = Vec::new();
    while let Some(notification) = notifications.next().await {
        if notification.uuid != TX {
            continue;
        }
        line.extend(notification.value);
        if let Some(end) = line.iter().position(|&b| b == b'\n') {
            return Ok(String::from_utf8_lossy(&line[..end]).trim().to_string());
        }
    }
    bail!("the badge disconnected during the handshake")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| text.get(i..i + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect::<Option<_>>()
        .ok_or_else(|| eyre!("bad hex {text:?} in the handshake"))
}

fn characteristic(peripheral: &Peripheral, uuid: Uuid) -> Result<btleplug::api::Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == uuid)
        .ok_or_else(|| eyre!("the badge has no characteristic {uuid}"))
}

/// Forward notifications to the reader and queued commands to the badge until either side ends.
async fn pump(
    peripheral: &Peripheral,
    data: &mpsc::Sender<io::Result<Vec<u8>>>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<()> {
    let rx = characteristic(peripheral, RX)?;
    let mut notifications = peripheral.notifications().await?;
    let mut liveness = tokio::time::interval(LIVENESS);
    loop {
        tokio::select! {
            _ = liveness.tick() => {
                if !peripheral.is_connected().await? {
                    bail!("the badge disconnected");
                }
            }
            notification = notifications.next() => {
                let Some(notification) = notification else { bail!("the badge disconnected") };
                if notification.uuid == TX && data.send(Ok(notification.value)).is_err() {
                    return Ok(());
                }
            }
            command = commands.recv() => {
                let Some(bytes) = command else { return Ok(()) };
                for chunk in bytes.chunks(WRITE_CHUNK) {
                    peripheral.write(&rx, chunk, WriteType::WithResponse).await?;
                }
            }
        }
    }
}
