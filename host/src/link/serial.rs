//! USB serial: the badge's CDC port, found by the Pico SDK's USB IDs.
use std::time::Duration;

use color_eyre::{
    Result,
    eyre::{OptionExt, WrapErr},
};
use serialport::SerialPortType;

use super::Link;

/// The badge firmware uses the Pico SDK's USB IDs so `picotool -f` can find it.
const VID: u16 = 0x2E8A;
const PID: u16 = 0x0009;

pub fn open(port: Option<String>) -> Result<Link> {
    let name = match port {
        Some(name) => name,
        None => serialport::available_ports()?
            .into_iter()
            .find(|p| matches!(&p.port_type, SerialPortType::UsbPort(usb) if usb.vid == VID && usb.pid == PID))
            // macOS lists each port as tty.* and cu.*; cu.* doesn't wait for carrier detect.
            .map(|p| p.port_name.replace("/dev/tty.", "/dev/cu."))
            .ok_or_eyre("badge not found: is it plugged in and running the Ratatui firmware?")?,
    };
    let mut port = serialport::new(&name, 115_200).timeout(Duration::from_millis(50)).open().wrap_err_with(|| format!("opening {name}"))?;
    // The badge only talks once the host raises DTR, as terminal programs do.
    port.write_data_terminal_ready(true)?;
    Ok(Link { reader: port.try_clone()?, writer: port, name, closed: None })
}
