//! The WebSocket relay (see `relay/`): the badge connects out over Wi-Fi and this joins the
//! same room. Binary messages carry the badge's output; text messages carry command lines.
use std::{
    io::{self, ErrorKind},
    net::TcpStream,
    sync::mpsc,
    thread,
    time::Duration,
};

use color_eyre::{Result, eyre::eyre};
use tungstenite::{
    Message, WebSocket,
    client::IntoClientRequest,
    http::{StatusCode, header::AUTHORIZATION},
    stream::MaybeTlsStream,
};

use super::{ChannelReader, ChannelWriter, Link};

pub fn connect(url: &str, token: &str) -> Result<Link> {
    let separator = if url.contains('?') { '&' } else { '?' };
    let mut request = format!("{url}{separator}role=host").into_client_request()?;
    request.headers_mut().insert(AUTHORIZATION, format!("Bearer {token}").parse()?);
    let mut socket = match tungstenite::connect(request) {
        Ok((socket, _)) => socket,
        Err(tungstenite::Error::Http(response)) if response.status() == StatusCode::UNAUTHORIZED => {
            return Err(eyre!("the relay rejected the token in badge.toml"));
        }
        Err(error) => return Err(eyre!("connecting to {url}: {error}")),
    };
    // Poll, so one thread can both read and send.
    let timeout = Some(Duration::from_millis(20));
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout)?,
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout)?,
        _ => {}
    }

    let (data_tx, data_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let error = pump(&mut socket, &data_tx, &command_rx).err();
        let _ = socket.close(None);
        let reason = error.map_or_else(|| "relay closed".to_string(), |e| e.to_string());
        let _ = data_tx.send(Err(io::Error::new(ErrorKind::BrokenPipe, reason)));
    });
    Ok(Link {
        reader: Box::new(ChannelReader::new(data_rx)),
        writer: Box::new(ChannelWriter(move |bytes| command_tx.send(bytes).is_ok())),
        name: format!("{url} (relay)"),
        closed: None,
    })
}

fn pump(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    data: &mpsc::Sender<io::Result<Vec<u8>>>,
    commands: &mpsc::Receiver<Vec<u8>>,
) -> Result<(), tungstenite::Error> {
    loop {
        loop {
            match commands.try_recv() {
                Ok(bytes) => socket.write(Message::text(String::from_utf8_lossy(&bytes).into_owned()))?,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match socket.flush() {
            Err(tungstenite::Error::Io(e)) if would_block(&e) => {}
            result => result?,
        }
        let bytes = match socket.read() {
            Ok(Message::Binary(bytes)) => bytes.to_vec(),
            Ok(Message::Text(text)) => text.as_bytes().to_vec(),
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e)) if would_block(&e) => continue,
            Err(error) => return Err(error),
        };
        if data.send(Ok(bytes)).is_err() {
            return Ok(());
        }
    }
}

fn would_block(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
