//! Internal I2C bus (GPIO18/19): LSM6DS3TR-C IMU at 0x6A and CAP1208 touch at 0x28.
//! Plus ADC battery (GPIO40, 750k/250k divider) and ambient light (GPIO43).
use embassy_rp::{
    adc::{self, Adc, Channel},
    i2c::{self, Blocking, I2c},
    peripherals::I2C1,
};

const IMU: u8 = 0x6A;
const CAP: u8 = 0x28;

pub struct Sensors {
    i2c: I2c<'static, I2C1, Blocking>,
    adc: Adc<'static, adc::Blocking>,
    battery: Channel<'static>,
    light: Channel<'static>,
    pub imu_ok: bool,
    pub touch_ok: bool,
}

impl Sensors {
    pub fn new(
        i2c: I2c<'static, I2C1, Blocking>,
        adc: Adc<'static, adc::Blocking>,
        battery: Channel<'static>,
        light: Channel<'static>,
    ) -> Self {
        let mut s = Self { i2c, adc, battery, light, imu_ok: false, touch_ok: false };
        // WHO_AM_I is 0x6A; then 104 Hz, +/-2 g, and block data update with auto-increment.
        let mut who = [0];
        s.imu_ok = s.i2c.blocking_write_read(IMU, &[0x0F], &mut who).is_ok()
            && who[0] == 0x6A
            && s.i2c.blocking_write(IMU, &[0x10, 0x40]).is_ok()
            && s.i2c.blocking_write(IMU, &[0x12, 0x44]).is_ok();
        // CAP1208 product ID register 0xFD reads 0x6B.
        let mut id = [0];
        s.touch_ok = s.i2c.blocking_write_read(CAP, &[0xFD], &mut id).is_ok() && id[0] == 0x6B;
        s
    }

    /// Acceleration in g.
    pub fn accel(&mut self) -> Option<[f32; 3]> {
        if !self.imu_ok {
            return None;
        }
        let mut raw = [0u8; 6];
        self.i2c.blocking_write_read(IMU, &[0x28], &mut raw).ok()?;
        Some(core::array::from_fn(|i| i16::from_le_bytes([raw[2 * i], raw[2 * i + 1]]) as f32 / 16384.0))
    }

    /// Touch bits, CAP1 = bit 0. Status latches until the INT bit is cleared.
    pub fn touch(&mut self) -> u8 {
        if !self.touch_ok {
            return 0;
        }
        let mut status = [0];
        if self.i2c.blocking_write_read(CAP, &[0x03], &mut status).is_err() {
            return 0;
        }
        let _ = self.i2c.blocking_write(CAP, &[0x00, 0x00]);
        status[0]
    }

    pub fn battery_v(&mut self) -> f32 {
        let raw = self.adc.blocking_read(&mut self.battery).unwrap_or(0);
        // The schematic notes say 750k/250k (x4), but readings on USB power (~8.4 V at x4)
        // only make sense at x2, so trust the measurement.
        raw as f32 * 3.3 / 4095.0 * 2.0
    }

    pub fn light(&mut self) -> f32 {
        self.adc.blocking_read(&mut self.light).unwrap_or(0) as f32 / 4095.0
    }
}

pub fn i2c_config() -> i2c::Config {
    let mut config = i2c::Config::default();
    config.frequency = 400_000;
    config
}
