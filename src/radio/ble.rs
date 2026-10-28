//! Bluetooth LE: the Nordic UART Service, a de facto standard serial port. Notifications carry
//! [`link::BLE`] to the host and writes carry command lines back, so `cargo badge --ble` speaks
//! the same protocol as over USB, and phone apps like nRF Connect work too.
// The GATT macros expand to borrows that clippy flags.
#![allow(clippy::needless_borrows_for_generic_args)]

use embassy_futures::{join::join, select::select};
use heapless::Vec;
use trouble_host::prelude::*;

use super::auth::{Auth, Step};
use crate::link::{self, LineBuffer};
// The GATT macros name `embassy_sync`, and trouble-host 0.6 needs 0.7 (we use 0.8 elsewhere).
use embassy_sync_07 as embassy_sync;

/// The largest notification with the default 251-byte packet pool (minus L2CAP and ATT headers).
const CHUNK: usize = 244;

#[gatt_server]
struct Server {
    uart: UartService,
}

#[gatt_service(uuid = "6e400001-b5a3-f393-e0a9-e50e24dcca9e")]
struct UartService {
    /// Host to badge: command lines.
    #[characteristic(
        uuid = "6e400002-b5a3-f393-e0a9-e50e24dcca9e",
        write,
        write_without_response
    )]
    rx: Vec<u8, CHUNK>,
    /// Badge to host: the log and mirror stream.
    #[characteristic(uuid = "6e400003-b5a3-f393-e0a9-e50e24dcca9e", notify)]
    tx: Vec<u8, CHUNK>,
}

/// The service UUID, least significant byte first, for the advertisement.
const UART_UUID: [u8; 16] = [
    0x9e, 0xca, 0xdc, 0x24, 0x0e, 0xe5, 0xa9, 0xe0, 0x93, 0xf3, 0xa3, 0xb5, 0x01, 0x00, 0x40, 0x6e,
];

pub async fn run(
    driver: cyw43::bluetooth::BtDriver<'_>,
    name: &'static str,
    key: Option<&'static str>,
    seed: [u8; 32],
) {
    let controller: ExternalController<_, 10> = ExternalController::new(driver);
    // A static random address from the chip ID, so hosts see the same badge across reboots.
    let id = embassy_rp::otp::get_chipid().unwrap_or(0).to_le_bytes();
    let address = Address::random([id[0], id[1], id[2], id[3], id[4], id[5] | 0xC0]);

    let mut resources: HostResources<DefaultPacketPool, 1, 2> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        mut runner,
        ..
    } = stack.build();
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name,
        appearance: &appearance::UNKNOWN,
    }))
    .unwrap();

    join(
        async {
            loop {
                let _ = runner.run().await;
            }
        },
        async {
            let mut connection = 0u32;
            loop {
                connection = connection.wrapping_add(1);
                link::write("ble: advertising\r\n");
                let Ok(conn) = advertise(name, &mut peripheral, &server).await else {
                    embassy_time::Timer::after_secs(1).await;
                    continue;
                };
                // Drop anything queued after the last host left (late replies, say), so this one
                // starts clean; a stale challenge would fail its handshake.
                link::BLE.disconnect();
                let auth = key.map(|key| Auth::new(key, &seed, connection));
                if auth.is_none() {
                    link::BLE.connect();
                }
                select(events(&server, &conn, auth), notify(&server, &conn)).await;
                link::BLE.disconnect();
            }
        },
    )
    .await;
}

async fn advertise<'v, 's, C: Controller>(
    name: &'v str,
    peripheral: &mut Peripheral<'v, C, DefaultPacketPool>,
    server: &'s Server<'v>,
) -> Result<GattConnection<'v, 's, DefaultPacketPool>, BleHostError<C::Error>> {
    // The 128-bit UUID fills most of the 31-byte advertisement, so the name goes in the scan
    // response. Hosts filter on the UUID.
    let mut adv = [0; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids128(&[UART_UUID]),
        ],
        &mut adv,
    )?;
    let mut scan = [0; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(name.as_bytes())],
        &mut scan,
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv[..adv_len],
                scan_data: &scan[..scan_len],
            },
        )
        .await?;
    Ok(advertiser.accept().await?.with_attribute_server(server)?)
}

/// Handle writes until the host disconnects. With a key, nothing but the handshake gets through
/// until the host has proved it knows the key.
async fn events<P: PacketPool>(server: &Server<'_>, conn: &GattConnection<'_, '_, P>, mut auth: Option<Auth>) {
    let mut line = LineBuffer::new();
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { .. } => return,
            GattConnectionEvent::Gatt { event } => {
                if let GattEvent::Write(write) = &event
                    && write.handle() == server.uart.rx.handle
                {
                    for &b in write.data() {
                        let Some(text) = line.push(b) else { continue };
                        let Some(handshake) = &mut auth else {
                            link::BLE.command(text).await;
                            continue;
                        };
                        match handshake.step(text) {
                            Step::Reply(reply) => link::BLE.reply(&reply),
                            Step::Accepted => {
                                auth = None;
                                link::BLE.connect();
                                link::BLE.reply("auth ok\r\n");
                            }
                            Step::Rejected => link::BLE.reply("auth failed\r\n"),
                            Step::Ignored => {}
                        }
                    }
                }
                if let Ok(reply) = event.accept() {
                    reply.send().await;
                }
            }
            _ => {}
        }
    }
}

/// Send queued output as notifications, as large as the negotiated MTU allows.
async fn notify<P: PacketPool>(server: &Server<'_>, conn: &GattConnection<'_, '_, P>) {
    let mut buf = [0; CHUNK];
    loop {
        let max = (conn.raw().att_mtu() as usize)
            .saturating_sub(3)
            .clamp(20, CHUNK);
        let n = link::BLE.read(&mut buf[..max]).await;
        let chunk = Vec::from_slice(&buf[..n]).unwrap();
        if server.uart.tx.notify(conn, &chunk).await.is_err() {
            return;
        }
    }
}
