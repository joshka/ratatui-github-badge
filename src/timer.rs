//! A monotonic countdown that never counts paused time.
#[derive(Clone, Copy)]
pub struct SessionTimer {
    pub remaining_ms: u64,
    pub running: bool,
    preset: usize,
}

impl Default for SessionTimer {
    fn default() -> Self {
        Self {
            remaining_ms: 30 * 60_000,
            running: false,
            preset: 1,
        }
    }
}

impl SessionTimer {
    const MINUTES: [u64; 4] = [15, 30, 45, 60];

    pub fn advance(&mut self, elapsed_ms: u64) {
        if self.running {
            self.remaining_ms = self.remaining_ms.saturating_sub(elapsed_ms);
            if self.remaining_ms == 0 {
                self.running = false;
            }
        }
    }

    pub fn toggle(&mut self) {
        if self.remaining_ms == 0 {
            self.reset();
        }
        self.running = !self.running;
    }

    pub fn set_preset(&mut self, forward: bool) {
        self.preset = (self.preset + if forward { 1 } else { 3 }) % Self::MINUTES.len();
        self.reset();
    }

    pub fn reset(&mut self) {
        self.remaining_ms = Self::MINUTES[self.preset] * 60_000;
        self.running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_resume_preserves_remaining_time() {
        let mut timer = SessionTimer::default();
        timer.toggle();
        timer.advance(1_234);
        timer.toggle();
        timer.advance(50_000);
        assert_eq!(timer.remaining_ms, 1_798_766);
        timer.toggle();
        timer.advance(766);
        assert_eq!(timer.remaining_ms, 1_798_000);
    }

    #[test]
    fn completion_stops_and_enter_restarts_preset() {
        let mut timer = SessionTimer::default();
        timer.toggle();
        timer.advance(u64::MAX);
        assert_eq!(timer.remaining_ms, 0);
        assert!(!timer.running);
        timer.toggle();
        assert_eq!(timer.remaining_ms, 30 * 60_000);
        assert!(timer.running);
    }

    #[test]
    fn changing_duration_resets_and_pauses() {
        let mut timer = SessionTimer::default();
        timer.toggle();
        timer.set_preset(false);
        assert_eq!(timer.remaining_ms, 15 * 60_000);
        assert!(!timer.running);
        timer.set_preset(false);
        assert_eq!(timer.remaining_ms, 60 * 60_000);
        timer.set_preset(true);
        assert_eq!(timer.remaining_ms, 15 * 60_000);
    }
}
