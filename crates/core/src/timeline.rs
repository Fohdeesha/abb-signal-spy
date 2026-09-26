//! Controller time.
//!
//! Every sample carries a controller-clock timestamp in milliseconds (the VC's is its
//! uptime; it ticks 4 per 4 ms sample, and 24 per 24 ms one). The charts and the
//! recorder run on that clock, not on when a frame happened to arrive, which is what
//! lets channels at different rates line up exactly and a dropped sample show as a
//! gap.
//!
//! Two things can go wrong with it, and both are handled here rather than in every
//! consumer:
//!
//! * **Width.** The stamp is a varint; if the controller keeps it in 32 bits it wraps
//!   after 49.7 days of uptime. A step back of more than half the 32-bit range, from
//!   a value inside it, is read as a wrap.
//!   A stamp from just before the wrap that arrives just after it (streams are a
//!   little out of step) belongs to the turn before, not to the next one.
//! * **Restarts.** A restarted controller's clock starts again from zero. The raw
//!   stamp is kept as the controller sent it (that is what a recording's
//!   `controller_ms` column promises), but the *timeline* the charts use is rebased
//!   so it keeps running forward: it resumes after the last point seen, plus the
//!   wall-clock time that passed. The reset is reported.
//! * **Jumps forward** further than the time that passed (a clock that was set, or
//!   one garbled stamp) are rebased the same way, so a single bad value cannot throw
//!   the timeline, and every chart with it, years ahead.

use std::time::{Duration, Instant, SystemTime};

const WRAP: u64 = 1 << 32;
/// Samples of different streams in one frame share a stamp, and a burst can deliver
/// streams a little out of step; a small step back is jitter, not a reset.
const JITTER_MS: u64 = 2_000;
/// How much further than the wall clock the controller's may move before it counts
/// as a jump: frames arrive in bursts, and the wall clock is read on arrival.
const FORWARD_SLACK_MS: u128 = 5_000;

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// What happened to the clock with this stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    None,
    /// The 32-bit clock wrapped.
    Wrapped,
    /// The clock went back: a controller restart. The timeline was rebased.
    Reset { from_raw: u64, to_raw: u64 },
}

/// Maps raw controller stamps to (unwrapped raw ms, timeline ms).
#[derive(Debug, Clone)]
pub struct Timeline {
    wraps: u64,
    last_raw: Option<u64>,
    /// timeline = unwrapped + offset
    offset: i64,
    last_timeline: Option<i64>,
    last_seen: Option<Instant>,
    /// The first timeline ms ever seen: the zero of every chart's time axis.
    origin: Option<i64>,
    /// Wall clock at a known timeline point, refreshed at every session start so the
    /// hover readout does not accumulate PC-versus-controller drift over hours.
    anchor: Option<(i64, SystemTime)>,
    /// (timeline ms from which it applies, offset): one entry per rebase, so a
    /// timeline point maps back to the controller's own clock exactly.
    segments: Vec<(i64, i64)>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    pub fn new() -> Timeline {
        Timeline { wraps: 0, last_raw: None, offset: 0, last_timeline: None, last_seen: None, origin: None, anchor: None, segments: Vec::new() }
    }

    /// The controller's clock (32-bit wraps undone) at a timeline point: what a
    /// recording's `controller_ms` column holds.
    pub fn controller_ms(&self, t: i64) -> i64 {
        let offset = self.segments.iter().rev().find(|(from, _)| t >= *from).map(|(_, o)| *o).unwrap_or(0);
        t.saturating_sub(offset)
    }

    /// A new session starts: the next stamp decides whether the controller's clock
    /// carried on (a reconnect) or started over (a restart). Nothing is lost here;
    /// the decision is made in [`Timeline::map`].
    pub fn session_start(&mut self) {
        self.anchor = None;
    }

    /// Map one stamp. `now` is when its frame arrived.
    pub fn map(&mut self, raw: u64, now: Instant) -> (u64, i64, ClockEvent) {
        let mut event = ClockEvent::None;
        let mut wraps = self.wraps;
        let mut straggler = false;
        if let Some(last) = self.last_raw {
            let elapsed = self.last_seen.map(|t| now.saturating_duration_since(t)).unwrap_or(Duration::ZERO);
            if raw < last {
                let back = last - raw;
                if last < WRAP && raw < WRAP && back > WRAP / 2 {
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
                if self.wraps > 0 && last < WRAP && raw < WRAP && ahead > WRAP / 2 {
                    // From just before the last wrap, arriving just after it.
                    wraps = self.wraps - 1;
                    straggler = true;
                } else if u128::from(ahead) > elapsed.as_millis() + elapsed.as_millis() / 10 + FORWARD_SLACK_MS {
                    self.rebase(raw, elapsed);
                    wraps = 0;
                    event = ClockEvent::Reset { from_raw: last, to_raw: raw };
                }
            }
        }
        // Keep the high-water mark, so jitter does not look like progress backwards.
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
        if self.anchor.is_none() {
            // The frame's arrival stands in for the sample's wall time: it is late
            // by the delivery latency, measured at tens of milliseconds at most.
            self.anchor = Some((t, SystemTime::now() - now.elapsed()));
        }
        (unwrapped, t, event)
    }

    /// The clock went back or leapt ahead: resume the timeline after the last point,
    /// plus the wall time that has passed since it.
    fn rebase(&mut self, raw: u64, elapsed: Duration) {
        // At least 1 ms on: the last point before the rebase keeps the old segment's
        // mapping back to the controller's clock.
        let resume_at = self.last_timeline.unwrap_or(0).saturating_add((elapsed.as_millis().min(i64::MAX as u128) as i64).max(1));
        self.wraps = 0;
        self.offset = resume_at.saturating_sub(to_i64(raw));
        self.segments.push((resume_at, self.offset));
    }

    /// Timeline ms to chart seconds.
    pub fn seconds(&self, t: i64) -> f64 {
        t.saturating_sub(self.origin.unwrap_or(t)) as f64 / 1000.0
    }

    pub fn origin(&self) -> Option<i64> {
        self.origin
    }

    /// The wall-clock time of a timeline point, from the latest anchor.
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
        // Jitter between streams: a small step back is not a reset.
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
        // And back to the controller's own clock, on both sides of the restart.
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
        // The next real stamp: the timeline carries on from where it was.
        let (_, t2, _) = tl.map(1008, t0 + Duration::from_millis(4));
        let (_, t3, e3) = tl.map(1012, t0 + Duration::from_millis(8));
        assert!((1004..1100).contains(&t2) && t3 == t2 + 4 && e3 == ClockEvent::None, "{t2} {t3} {e3:?}");
        // A jump no bigger than the time that passed is just time passing.
        let later = t0 + Duration::from_secs(3600);
        let (_, t4, e4) = tl.map(1012 + 3_600_000, later);
        assert_eq!((t4, e4), (t3 + 3_600_000, ClockEvent::None));
    }

    #[test]
    fn a_rebase_leaves_the_point_before_it_mapped() {
        // A restart seen within the same instant as the last point: no time passed.
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
        // Another stream's sample from just before the wrap.
        let (u, t, e) = tl.map(WRAP - 4, now);
        assert_eq!((u, t, e), (WRAP - 4, (WRAP - 4) as i64, ClockEvent::None), "it belongs to the turn before");
        let (u, t, e) = tl.map(4, now);
        assert_eq!((u, t, e), (WRAP + 4, (WRAP + 4) as i64, ClockEvent::None), "and the clock carries on after the wrap");
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
