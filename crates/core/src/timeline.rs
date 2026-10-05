use std::time::{Duration, Instant, SystemTime};

const WRAP: u64 = 1 << 32;
const JITTER_MS: u64 = 2_000;
const FORWARD_SLACK_MS: u128 = 5_000;

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    None,
    Wrapped,
    Reset { from_raw: u64, to_raw: u64 },
}

#[derive(Debug, Clone)]
pub struct Timeline {
    wraps: u64,
    last_raw: Option<u64>,
    offset: i64,
    last_timeline: Option<i64>,
    last_seen: Option<Instant>,
    origin: Option<i64>,
    anchor: Option<(i64, SystemTime)>,
    refresh_anchor: bool,
    segments: Vec<(i64, i64)>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    pub fn new() -> Timeline {
        Timeline { wraps: 0, last_raw: None, offset: 0, last_timeline: None, last_seen: None, origin: None, anchor: None, refresh_anchor: false, segments: Vec::new() }
    }

    pub fn controller_ms(&self, t: i64) -> i64 {
        let offset = self.segments.iter().rev().find(|(from, _)| t >= *from).map(|(_, o)| *o).unwrap_or(0);
        t.saturating_sub(offset)
    }

    pub fn segment_start(&self, t: i64) -> i64 {
        self.segments.iter().rev().find(|(from, _)| t >= *from).map(|(from, _)| *from).unwrap_or(i64::MIN)
    }

    pub fn session_start(&mut self) {
        self.refresh_anchor = true;
    }

    pub fn map(&mut self, raw: u64, now: Instant) -> (u64, i64, ClockEvent) {
        let mut event = ClockEvent::None;
        let mut wraps = self.wraps;
        let mut straggler = false;
        if let Some(last) = self.last_raw {
            let elapsed = self.last_seen.map(|t| now.saturating_duration_since(t)).unwrap_or(Duration::ZERO);
            let fits = |step: u64| u128::from(step) <= elapsed.as_millis() + elapsed.as_millis() / 10 + FORWARD_SLACK_MS;
            if raw < last {
                let back = last - raw;
                if last < WRAP && raw < WRAP && back > WRAP / 2 && fits(WRAP - last + raw) {
                    self.wraps += 1;
                    wraps = self.wraps;
                    event = ClockEvent::Wrapped;
                } else if back > JITTER_MS {
                    self.rebase(raw, elapsed);
                    wraps = 0;
                    event = ClockEvent::Reset { from_raw: last, to_raw: raw };
                }
            } else {
                let ahead = raw - last;
                if self.wraps > 0 && last < WRAP && raw < WRAP && ahead > WRAP / 2 && WRAP - ahead <= JITTER_MS {
                    wraps = self.wraps - 1;
                    straggler = true;
                } else if !fits(ahead) {
                    self.rebase(raw, elapsed);
                    wraps = 0;
                    event = ClockEvent::Reset { from_raw: last, to_raw: raw };
                }
            }
        }
        if event != ClockEvent::None || (!straggler && self.last_raw.is_none_or(|l| raw > l)) {
            self.last_raw = Some(raw);
        }
        let unwrapped = raw.saturating_add(wraps.saturating_mul(WRAP));
        let t = to_i64(unwrapped).saturating_add(self.offset);
        if self.last_timeline.is_none_or(|l| t > l) || event != ClockEvent::None {
            self.last_timeline = Some(t);
            self.last_seen = Some(now);
        }
        if self.origin.is_none() {
            self.origin = Some(t);
        }
        if self.anchor.is_none() || self.refresh_anchor {
            self.anchor = Some((t, SystemTime::now() - now.elapsed()));
            self.refresh_anchor = false;
        }
        (unwrapped, t, event)
    }

    fn rebase(&mut self, raw: u64, elapsed: Duration) {
        let last = self.last_timeline.unwrap_or(0);
        let resume_at = last.saturating_add((elapsed.as_millis().min(i64::MAX as u128) as i64).max(1));
        self.wraps = 0;
        self.offset = resume_at.saturating_sub(to_i64(raw));
        let from = resume_at.saturating_sub(JITTER_MS as i64).max(last.saturating_add(1)).min(resume_at);
        self.segments.push((from, self.offset));
    }

    pub fn seconds(&self, t: i64) -> f64 {
        t.saturating_sub(self.origin.unwrap_or(t)) as f64 / 1000.0
    }

    pub fn origin(&self) -> Option<i64> {
        self.origin
    }

    pub fn wall(&self, t: i64) -> Option<SystemTime> {
        let (at, wall) = self.anchor?;
        let d = t.saturating_sub(at);
        if d >= 0 { wall.checked_add(Duration::from_millis(d as u64)) } else { wall.checked_sub(Duration::from_millis(d.unsigned_abs())) }
    }

    pub fn anchor(&self) -> Option<(i64, SystemTime)> {
        self.anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_clock_maps_straight_through() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        assert_eq!(tl.map(1000, now), (1000, 1000, ClockEvent::None));
        assert_eq!(tl.map(1004, now), (1004, 1004, ClockEvent::None));
        assert_eq!(tl.map(1000, now), (1000, 1000, ClockEvent::None));
        assert_eq!(tl.map(1008, now), (1008, 1008, ClockEvent::None));
        assert_eq!(tl.seconds(1008), 0.008);
    }

    #[test]
    fn a_32_bit_wrap_carries_on() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        let near = WRAP - 4;
        tl.map(near, now);
        let (u, t, e) = tl.map(0, now);
        assert_eq!(e, ClockEvent::Wrapped);
        assert_eq!(u, WRAP);
        assert_eq!(t, WRAP as i64);
        let (u, _, e) = tl.map(4, now);
        assert_eq!((u, e), (WRAP + 4, ClockEvent::None));
    }

    #[test]
    fn a_restart_rebases_forward() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(128_434_653, t0);
        let (_, before, _) = tl.map(128_434_657, t0);
        tl.session_start();
        let later = t0 + Duration::from_secs(90);
        let (u, t, e) = tl.map(12_000, later);
        assert_eq!(e, ClockEvent::Reset { from_raw: 128_434_657, to_raw: 12_000 });
        assert_eq!(u, 12_000, "the raw stamp is kept as sent");
        assert_eq!(t, before + 90_000, "the timeline resumes after the gap");
        let (_, t2, e) = tl.map(12_004, later);
        assert_eq!((t2, e), (t + 4, ClockEvent::None));
        assert_eq!(tl.controller_ms(before), 128_434_657);
        assert_eq!(tl.controller_ms(t2), 12_004);
    }

    #[test]
    fn a_reconnect_without_restart_keeps_the_clock() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(5_000, t0);
        tl.session_start();
        let (_, t, e) = tl.map(9_000, t0 + Duration::from_secs(4));
        assert_eq!((t, e), (9_000, ClockEvent::None));
    }

    #[test]
    fn one_garbled_stamp_cannot_throw_the_timeline_ahead() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(1000, t0);
        tl.map(1004, t0);
        let (u, t, e) = tl.map(1 << 40, t0);
        assert_eq!(u, 1 << 40, "the raw stamp is kept as sent");
        assert!(matches!(e, ClockEvent::Reset { .. }), "{e:?}");
        assert!(t <= 1004 + 10, "the timeline leapt to {t}");
        let (_, t2, _) = tl.map(1008, t0 + Duration::from_millis(4));
        let (_, t3, e3) = tl.map(1012, t0 + Duration::from_millis(8));
        assert!((1004..1100).contains(&t2) && t3 == t2 + 4 && e3 == ClockEvent::None, "{t2} {t3} {e3:?}");
        let later = t0 + Duration::from_secs(3600);
        let (_, t4, e4) = tl.map(1012 + 3_600_000, later);
        assert_eq!((t4, e4), (t3 + 3_600_000, ClockEvent::None));
    }

    #[test]
    fn a_rebase_leaves_the_point_before_it_mapped() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        tl.map(5_000, now);
        let (_, t, _) = tl.map(5_004, now);
        let (_, t2, e) = tl.map(10, now);
        assert!(matches!(e, ClockEvent::Reset { .. }));
        assert!(t2 > t);
        assert_eq!(tl.controller_ms(t), 5_004, "the last point before the restart maps to its own clock");
        assert_eq!(tl.controller_ms(t2), 10);
    }

    #[test]
    fn a_stamp_from_before_a_wrap_arriving_after_it() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        tl.map(WRAP - 8, now);
        tl.map(WRAP - 4, now);
        let (u, _, e) = tl.map(0, now);
        assert_eq!((u, e), (WRAP, ClockEvent::Wrapped));
        let (u, t, e) = tl.map(WRAP - 4, now);
        assert_eq!((u, t, e), (WRAP - 4, (WRAP - 4) as i64, ClockEvent::None), "it belongs to the turn before");
        let (u, t, e) = tl.map(4, now);
        assert_eq!((u, t, e), (WRAP + 4, (WRAP + 4) as i64, ClockEvent::None), "and the clock carries on after the wrap");
    }

    #[test]
    fn a_restart_past_half_the_32_bit_range_is_a_restart_not_a_wrap() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(3_706_265_421, t0);
        let (_, before, _) = tl.map(3_706_265_425, t0);
        tl.session_start();
        let later = t0 + Duration::from_secs(120);
        let (u, t, e) = tl.map(90_000, later);
        assert_eq!(e, ClockEvent::Reset { from_raw: 3_706_265_425, to_raw: 90_000 });
        assert_eq!(u, 90_000, "the raw stamp is kept as sent, not moved a turn ahead");
        assert_eq!(t, before + 120_000, "the timeline resumes after the two minutes that passed");
        let (u, t2, e) = tl.map(90_004, later);
        assert_eq!((u, t2, e), (90_004, t + 4, ClockEvent::None));
    }

    #[test]
    fn a_wrap_after_a_long_absence_is_still_a_wrap() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(WRAP - 1_000, t0);
        let (u, _, e) = tl.map(60_000, t0 + Duration::from_secs(61));
        assert_eq!((u, e), (WRAP + 60_000, ClockEvent::Wrapped));
    }

    #[test]
    fn one_garbled_stamp_inside_the_32_bit_range_cannot_throw_the_timeline_ahead() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(1000, t0);
        tl.map(1004, t0);
        let (_, t, e) = tl.map(3_000_000_000, t0);
        assert!(matches!(e, ClockEvent::Reset { .. }), "{e:?}");
        assert!(t <= 1004 + 10, "the timeline leapt to {t}");
        let (u, t2, e2) = tl.map(1008, t0 + Duration::from_millis(4));
        assert_eq!(u, 1008, "the raw stamp was moved a turn ahead ({e2:?})");
        assert!((1004..=2100).contains(&t2), "the timeline went to {t2} ({e2:?})");
        let (_, t3, e3) = tl.map(1012, t0 + Duration::from_millis(8));
        assert_eq!((t3, e3), (t2 + 4, ClockEvent::None));
    }

    #[test]
    fn after_a_wrap_a_big_jump_forward_is_rebased_not_taken_for_a_straggler() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(WRAP - 4, t0);
        let (_, tw, e) = tl.map(0, t0);
        assert_eq!(e, ClockEvent::Wrapped);
        let later = t0 + Duration::from_secs(60);
        let (_, t1, e1) = tl.map(3_000_000_000, later);
        assert!(matches!(e1, ClockEvent::Reset { .. }), "{e1:?}");
        assert!(t1 > tw, "the timeline went back: {t1} after {tw}");
        let (_, t2, e2) = tl.map(3_000_000_004, later + Duration::from_millis(4));
        assert_eq!((t2, e2), (t1 + 4, ClockEvent::None), "and it carries on from there, so the store accepts the samples");
    }

    #[test]
    fn the_wall_clock_stays_known_while_reconnecting() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(1000, t0);
        tl.session_start();
        assert!(tl.wall(1000).is_some(), "no wall clock between the connect and the first sample");
        tl.map(9000, t0 + Duration::from_secs(8));
        assert_eq!(tl.anchor().map(|a| a.0), Some(9000), "the anchor is refreshed at the new session's first stamp");
    }

    #[test]
    fn a_stamp_a_little_behind_a_restart_maps_to_the_new_clock() {
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        tl.map(500_000, t0);
        tl.map(500_004, t0);
        let later = t0 + Duration::from_secs(90);
        let (_, t, e) = tl.map(10_004, later);
        assert!(matches!(e, ClockEvent::Reset { .. }), "{e:?}");
        let (u, t_lag, e) = tl.map(10_000, later);
        assert_eq!((u, e), (10_000, ClockEvent::None));
        assert_eq!(t_lag, t - 4);
        assert_eq!(tl.controller_ms(t_lag), 10_000, "mapped back with the old clock's offset");
        assert_eq!(tl.controller_ms(t), 10_004);
        assert_eq!(tl.segment_start(t_lag), tl.segment_start(t), "both in the new clock's segment");
    }

    #[test]
    fn extreme_stamps_do_not_overflow() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        for raw in [u64::MAX, 0, u64::MAX - 1, 1 << 63, 5, u64::MAX] {
            let (_, t, _) = tl.map(raw, now);
            let _ = (tl.controller_ms(t), tl.seconds(t), tl.wall(t));
        }
    }

    #[test]
    fn wall_clock_from_the_anchor() {
        let mut tl = Timeline::new();
        let now = Instant::now();
        tl.map(1000, now);
        let w0 = tl.wall(1000).unwrap();
        let w1 = tl.wall(3500).unwrap();
        assert_eq!(w1.duration_since(w0).unwrap(), Duration::from_millis(2500));
        assert!(tl.wall(0).is_some());
    }
}
