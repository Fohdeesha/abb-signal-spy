pub const ZERO_HOLD_MS: f64 = 100.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct ZeroHold {
    held: Option<(i64, f64)>,
}

impl ZeroHold {
    pub fn new() -> ZeroHold {
        ZeroHold::default()
    }

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
        let mut h = ZeroHold::new();
        assert_eq!(h.apply_at(396, 0.5), 0.5);
        assert_eq!(h.apply_at(5396, 0.0), 0.0, "a value from before a 5 s gap carried across it");
        assert_eq!(h.apply_at(5400, 0.0), 0.0);
    }
}
