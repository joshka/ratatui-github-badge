//! The four white case LEDs on GPIO0-3 (through 330 ohm), dimmed with PWM.
use embassy_rp::pwm::{Config, Pwm};

pub struct Leds {
    left: Pwm<'static>,
    right: Pwm<'static>,
    config: Config,
}

impl Leds {
    /// `left` drives GPIO0/1 (PWM slice 0), `right` drives GPIO2/3 (slice 1).
    pub fn new(left: Pwm<'static>, right: Pwm<'static>) -> Self {
        let mut config = Config::default();
        config.top = 0xFFFF;
        Self { left, right, config }
    }

    pub fn config() -> Config {
        let mut config = Config::default();
        config.top = 0xFFFF;
        config
    }

    /// Linear 0-255 brightness; squared so steps look even to the eye.
    pub fn set(&mut self, levels: [u8; 4]) {
        let duty = |v: u8| (v as u32 * v as u32 * 0xFFFF / (255 * 255)) as u16;
        self.config.compare_a = duty(levels[0]);
        self.config.compare_b = duty(levels[1]);
        self.left.set_config(&self.config);
        self.config.compare_a = duty(levels[2]);
        self.config.compare_b = duty(levels[3]);
        self.right.set_config(&self.config);
    }
}
