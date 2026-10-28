//! The Wi-Fi relay: join a `[[wifi]]` network and hold a WebSocket open to the relay (see
//! `relay/`), which forwards it to `cargo badge --relay`. Connecting out works through client
//! isolation, NAT and most filtering, which a laptop connecting in to the badge would not.
//!
//! Binary messages carry [`link::RELAY`] to the host; text messages carry command lines back.
//!
//! TLS doesn't verify the server certificate (there's no certificate store on the badge), so
//! someone able to intercept the connection could read the token and the mirror stream.
use core::cell::RefCell;

use badge_ui::config::{Relay, WIFI};
use cyw43::{Control, JoinOptions, NetDriver};
use embassy_futures::{
    join::join,
    select::{Either, select},
};
use embassy_net::{
    Config, Stack, StackResources,
    dns::DnsQueryType,
    tcp::{TcpReader, TcpSocket, TcpWriter},
};
use embassy_time::{Duration, Timer};
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use rand_core::RngCore;
use static_cell::{ConstStaticCell, StaticCell};

use crate::link::{self, LineBuffer};

pub async fn run(
    net: NetDriver<'static>,
    control: &mut Control<'static>,
    rng: &mut (impl RngCore + rand_core::CryptoRng),
    config: Relay,
) {
    let Some(url) = Url::parse(config.url) else {
        return;
    };
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let (stack, mut runner) = embassy_net::new(
        net,
        Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        rng.next_u64(),
    );
    // Cloudflare won't shrink its TLS records, so reads need room for a full 16 KB one.
    static TLS_READ: ConstStaticCell<[u8; 16640]> = ConstStaticCell::new([0; 16640]);
    static TLS_WRITE: ConstStaticCell<[u8; 4096]> = ConstStaticCell::new([0; 4096]);
    let mut tls_buffers = url.tls.then(|| (TLS_READ.take(), TLS_WRITE.take()));

    join(runner.run(), async {
        let mut backoff = Duration::from_secs(2);
        loop {
            if !stack.is_link_up() {
                join_wifi(control).await;
            }
            stack.wait_config_up().await;
            let buffers = tls_buffers.as_mut().map(|(r, w)| (&mut r[..], &mut w[..]));
            if let Err(error) = session(stack, &url, config.token, buffers, rng).await {
                link::write(&alloc::format!("relay: {error}\r\n"));
            }
            link::RELAY.disconnect();
            Timer::after(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    })
    .await;
}

/// Try each configured network in order until one accepts us.
async fn join_wifi(control: &mut Control<'static>) {
    loop {
        for wifi in WIFI {
            let options = match wifi.password {
                Some(password) => JoinOptions::new(password.as_bytes()),
                None => JoinOptions::new_open(),
            };
            match control.join(wifi.ssid, options).await {
                Ok(()) => {
                    link::write(&alloc::format!("wifi: joined {}\r\n", wifi.ssid));
                    return;
                }
                Err(error) => link::write(&alloc::format!("wifi: couldn't join {}: {error:?}\r\n", wifi.ssid)),
            }
        }
        Timer::after_secs(10).await;
    }
}

/// `ws[s]://host[:port]/path`
struct Url {
    tls: bool,
    host: &'static str,
    port: u16,
    path: &'static str,
}

impl Url {
    fn parse(url: &'static str) -> Option<Self> {
        let (tls, rest) = match url.split_once("://")? {
            ("wss", rest) => (true, rest),
            ("ws", rest) => (false, rest),
            _ => return None,
        };
        let (authority, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, if tls { 443 } else { 80 }),
        };
        Some(Self {
            tls,
            host,
            port,
            path,
        })
    }
}

async fn session(
    stack: Stack<'static>,
    url: &Url,
    token: &str,
    tls_buffers: Option<(&mut [u8], &mut [u8])>,
    rng: &mut (impl RngCore + rand_core::CryptoRng),
) -> Result<(), &'static str> {
    let address = *stack
        .dns_query(url.host, DnsQueryType::A)
        .await
        .map_err(|_| "DNS failed")?
        .first()
        .ok_or("no address")?;
    let (mut rx, mut tx) = ([0; 2048], [0; 2048]);
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(90)));
    socket
        .connect((address, url.port))
        .await
        .map_err(|_| "connect failed")?;
    let (reader, writer) = socket.split();
    let halves = Halves {
        reader: &RefCell::new(reader),
        writer: &RefCell::new(writer),
    };
    let mut mask = rng.next_u32() | 1;

    let Some((read_buf, write_buf)) = tls_buffers else {
        return websocket(halves, halves, url, token, &mut mask).await;
    };
    let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(halves, read_buf, write_buf);
    let config = TlsConfig::new().with_server_name(url.host);
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(&mut *rng),
    ))
    .await
    .map_err(|_| "TLS handshake failed")?;
    let (reader, writer) = tls.split();
    websocket(reader, writer, url, token, &mut mask).await
}

/// Upgrade to a WebSocket, then pump bytes both ways until either side fails.
async fn websocket<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    url: &Url,
    token: &str,
    mask: &mut u32,
) -> Result<(), &'static str> {
    let request = alloc::format!(
        "GET {path}?role=badge HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
         Authorization: Bearer {token}\r\n\r\n",
        path = url.path,
        host = url.host,
    );
    writer
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "write failed")?;
    writer.flush().await.map_err(|_| "write failed")?;
    // Read the response a byte at a time so no WebSocket data after it is lost.
    let mut response = heapless::Vec::<u8, 512>::new();
    while !response.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        reader
            .read_exact(&mut byte)
            .await
            .map_err(|_| "no response")?;
        response.push(byte[0]).map_err(|_| "response too long")?;
    }
    if !response.starts_with(b"HTTP/1.1 101") {
        return Err(if response.starts_with(b"HTTP/1.1 401") {
            "token rejected"
        } else {
            "upgrade refused"
        });
    }
    link::RELAY.connect();
    link::write("relay: connected\r\n");
    match select(receive(&mut reader), send(&mut writer, mask)).await {
        Either::First(result) | Either::Second(result) => result,
    }
}

/// Feed text and binary messages from the host through the command parser.
async fn receive<R: Read>(reader: &mut R) -> Result<(), &'static str> {
    let mut line = LineBuffer::new();
    loop {
        let mut header = [0; 2];
        reader
            .read_exact(&mut header)
            .await
            .map_err(|_| "read failed")?;
        let opcode = header[0] & 0x0F;
        let mut len = u64::from(header[1] & 0x7F);
        if len == 126 {
            let mut ext = [0; 2];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|_| "read failed")?;
            len = u64::from(u16::from_be_bytes(ext));
        } else if len == 127 {
            let mut ext = [0; 8];
            reader
                .read_exact(&mut ext)
                .await
                .map_err(|_| "read failed")?;
            len = u64::from_be_bytes(ext);
        }
        if opcode == 0x8 {
            return Err("closed by relay");
        }
        let mut chunk = [0; 64];
        while len > 0 {
            let n = (len as usize).min(chunk.len());
            reader
                .read_exact(&mut chunk[..n])
                .await
                .map_err(|_| "read failed")?;
            len -= n as u64;
            if opcode <= 0x2 {
                for &b in &chunk[..n] {
                    if let Some(text) = line.push(b) {
                        link::RELAY.command(text).await;
                    }
                }
            }
        }
    }
}

/// Send queued output as masked binary messages, as the protocol requires of clients.
async fn send<W: Write>(writer: &mut W, mask: &mut u32) -> Result<(), &'static str> {
    let mut frame = [0; 8 + 1024];
    loop {
        let n = link::RELAY.read(&mut frame[8..]).await;
        // xorshift: the mask only has to be unpredictable to proxies, not secret.
        *mask ^= *mask << 13;
        *mask ^= *mask >> 17;
        *mask ^= *mask << 5;
        let key = mask.to_be_bytes();
        let [hi, lo] = (n as u16).to_be_bytes();
        frame[..8].copy_from_slice(&[0x82, 0x80 | 126, hi, lo, key[0], key[1], key[2], key[3]]);
        for (i, b) in frame[8..8 + n].iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
        writer
            .write_all(&frame[..8 + n])
            .await
            .map_err(|_| "write failed")?;
        writer.flush().await.map_err(|_| "write failed")?;
    }
}

/// TLS wants a socket it can clone into a reader and a writer. Each clone only ever reads or
/// only ever writes, so the two `RefCell`s are never borrowed twice.
#[derive(Clone, Copy)]
struct Halves<'a, 's> {
    reader: &'a RefCell<TcpReader<'s>>,
    writer: &'a RefCell<TcpWriter<'s>>,
}

impl ErrorType for Halves<'_, '_> {
    type Error = embassy_net::tcp::Error;
}

#[allow(clippy::await_holding_refcell_ref)]
impl Read for Halves<'_, '_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.reader.borrow_mut().read(buf).await
    }
}

#[allow(clippy::await_holding_refcell_ref)]
impl Write for Halves<'_, '_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.writer.borrow_mut().write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.borrow_mut().flush().await
    }
}
