//! The ways to reach the badge. Each gives a byte stream in and out, so the mirror and the
//! one-shot commands don't care which they use.
use std::{
    io::{self, Read, Write},
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

mod ble;
mod relay;
mod serial;

pub use ble::connect as ble;
pub use relay::connect as relay;
pub use serial::open as serial;

pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Shown in the mirror's title.
    pub name: String,
    /// Ends once the link has hung up, after the writer is dropped. Bluetooth needs this: if
    /// the process exits first, the badge thinks it is still connected and stops advertising.
    pub closed: Option<Receiver<()>>,
}

impl Link {
    /// Hang up, waiting a moment for the transport to tell the badge.
    pub fn close(writer: Box<dyn Write + Send>, closed: Option<Receiver<()>>) {
        drop(writer);
        if let Some(closed) = closed {
            let _ = closed.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// Bytes from a transport thread. Reads time out like the serial port's, so callers can poll.
struct ChannelReader {
    rx: Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
}

impl ChannelReader {
    fn new(rx: Receiver<io::Result<Vec<u8>>>) -> Self {
        Self { rx, buf: Vec::new(), pos: 0 }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(bytes)) => (self.buf, self.pos) = (bytes, 0),
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Bytes for a transport thread.
struct ChannelWriter<F: FnMut(Vec<u8>) -> bool>(F);

impl<F: FnMut(Vec<u8>) -> bool> Write for ChannelWriter<F> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if (self.0)(buf.to_vec()) { Ok(buf.len()) } else { Err(io::ErrorKind::BrokenPipe.into()) }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
