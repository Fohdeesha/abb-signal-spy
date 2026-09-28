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
/// of zeros is a real zero (the joint at rest). By time, not by count: a gap (a
/// reconnect, a pause) longer than the hold never carries a value across it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZeroHold {
    /// The last value reported, and when.
    held: Option<(i64, f64)>,
}

impl ZeroHold {
    pub fn new() -> ZeroHold {
        ZeroHold::default()
    }

    /// The value the sample at `t_ms` stands for.
    pub fn apply_at(&mut self, t_ms: i64, v: f64) -> f64 {
        if !v.is_finite() {
            return v;
        }
        if v != 0.0 {
            self.held = Some((t_ms, v));
            return v;
        }
        match self.held {
            Some((at, held)) if (t_ms - at) as f64 <= ZERO_HOLD_MS => held,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_stands_for_a_value_only_within_the_hold_time_of_it() {
        let mut h = ZeroHold::new();
        assert_eq!(h.apply_at(396, 0.5), 0.5);
        assert_eq!(h.apply_at(400, 0.0), 0.5, "padding right after a value");
        assert_eq!(h.apply_at(496, 0.0), 0.5, "held for the hold time");
        assert_eq!(h.apply_at(500, 0.0), 0.0, "and not beyond it");
        // A gap (a reconnect), then zeros: the joint at rest, not the speed from before.
        let mut h = ZeroHold::new();
        assert_eq!(h.apply_at(396, 0.5), 0.5);
        assert_eq!(h.apply_at(5396, 0.0), 0.0, "a value from before a 5 s gap carried across it");
        assert_eq!(h.apply_at(5400, 0.0), 0.0);
    }
}
