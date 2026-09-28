//! How some signals must be read: shared by the live window and a recording opened
//! for review.

/// How long a zero-filled signal's padding may run (the joint speeds 6010-6015 and
/// 6030-6035 carry a value every few samples and exact zeros between). Measured on
/// the cell (2026-09-02 motion data, all twelve joint-speed signals, every joint
/// moving): the longest run of padding zeros inside motion was 11 samples, 44 ms.
/// Twice that and more; a joint that stops reads 0 within it.
pub const ZERO_HOLD_MS: f64 = 100.0;

/// Undoes a zero-filled signal's padding, sample by sample in time order: a zero
/// within [`ZERO_HOLD_MS`] of a reported value stands for that value; a longer run
/// of zeros is a real zero (the joint at rest).
#[derive(Debug, Clone, Copy)]
pub struct ZeroHold {
    held: f64,
    zeros: u32,
    max: u32,
}

impl ZeroHold {
    pub fn new(sample_ms: f64) -> ZeroHold {
        let ms = if sample_ms.is_finite() && sample_ms > 0.1 { sample_ms } else { 4.032 };
        ZeroHold { held: 0.0, zeros: 0, max: (ZERO_HOLD_MS / ms).ceil() as u32 }
    }

    pub fn apply(&mut self, v: f64) -> f64 {
        if !v.is_finite() {
            return v;
        }
        if v != 0.0 {
            self.held = v;
            self.zeros = 0;
            return v;
        }
        self.zeros = self.zeros.saturating_add(1);
        if self.zeros > self.max {
            self.held = 0.0;
        }
        self.held
    }
}
