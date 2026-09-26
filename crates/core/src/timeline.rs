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
//!   a value inside it, is read as a wrap when the step forward it implies fits the
//!   wall time that passed; otherwise it is a restart (below).
//!   A stamp from just before the wrap that arrives just after it (streams are a
//!   little out of step) belongs to the turn before, not to the next one.
//!   Whether the IRC5 wraps at all is unmeasured: the cell's clock was at 3.7e9 ms on
//!   2026-09-26 and reaches 2^32 about a week later if it is not restarted.
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
    /// The next stamp replaces the anchor (a new session began). The old one serves
    /// until then: the timeline runs on with the wall clock across the gap.
    refresh_anchor: bool,
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
        Timeline { wraps: 0, last_raw: None, offset: 0, last_timeline: None, last_seen: None, origin: None, anchor: None, refresh_anchor: false, segments: Vec::new() }
    }

    /// The controller's clock (32-bit wraps undone) at a timeline point: what a
    /// recording's `controller_ms` column holds.
    pub fn controller_ms(&self, t: i64) -> i64 {
        let offset = self.segments.iter().rev().find(|(from, _)| t >= *from).map(|(_, o)| *o).unwrap_or(0);
        t.saturating_sub(offset)
    }

    /// Where the clock segment holding timeline point `t` begins (`i64::MIN` for the
    /// first): points in one segment share one mapping back to the controller's
    /// clock, and a new segment begins at every restart or jump.
    pub fn segment_start(&self, t: i64) -> i64 {
        self.segments.iter().rev().find(|(from, _)| t >= *from).map(|(from, _)| *from).unwrap_or(i64::MIN)
    }

    /// A new session starts: the next stamp decides whether the controller's clock
    /// carried on (a reconnect) or started over (a restart). Nothing is lost here;
    /// the decision is made in [`Timeline::map`].
    pub fn session_start(&mut self) {
        self.refresh_anchor = true;
    }

    /// Map one stamp. `now` is when its frame arrived.
    pub fn map(&mut self, raw: u64, now: Instant) -> (u64, i64, ClockEvent) {
        let mut event = ClockEvent::None;
        let mut wraps = self.wraps;
        let mut straggler = false;
        if let Some(last) = self.last_raw {
            let elapsed = self.last_seen.map(|t| now.saturating_duration_since(t)).unwrap_or(Duration::ZERO);
            // Whether the controller's clock could have moved `step` ms forward in the
            // wall time that passed.
            let fits = |step: u64| u128::from(step) <= elapsed.as_millis() + elapsed.as_millis() / 10 + FORWARD_SLACK_MS;
            if raw < last {
                let back = last - raw;
                // A wrap only when the step forward it implies fits the time that
                // passed: a restart of a controller whose clock was past 2^31 (the
                // measured cell's was, at 42.9 days) looks the same otherwise.
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
                // From just before the last wrap, arriving just after it: then it lies
                // within the jitter behind the newest stamp, across the wrap.
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
        if self.anchor.is_none() || self.refresh_anchor {
            // The frame's arrival stands in for the sample's wall time: it is late
            // by the delivery latency, measured at tens of milliseconds at most.
            self.anchor = Some((t, SystemTime::now() - now.elapsed()));
            self.refresh_anchor = false;
        }
        (unwrapped, t, event)
    }

    /// The clock went back or leapt ahead: resume the timeline after the last point,
    /// plus the wall time that has passed since it.
    fn rebase(&mut self, raw: u64, elapsed: Duration) {
        // At least 1 ms on: the last point before the rebase keeps the old segment's
        // mapping back to the controller's clock.
        let last = self.last_timeline.unwrap_or(0);
        let resume_at = last.saturating_add((elapsed.as_millis().min(i64::MAX as u128) as i64).max(1));
        self.wraps = 0;
        self.offset = resume_at.saturating_sub(to_i64(raw));
        // The new segment reaches back into the gap by up to the jitter, so another
        // stream's stamp a tick behind the one that showed the restart maps with the
        // new clock too; never back over the old segment's last point.
        let from = resume_at.saturating_sub(JITTER_MS as i64).max(last.saturating_add(1)).min(resume_at);
        self.segments.push((from, self.offset));
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
    fn a_restart_past_half_the_32_bit_range_is_a_restart_not_a_wrap() {
        // The measured cell's clock read 3,706,265,421 ms (42.9 days) on 2026-09-26:
        // past 2^31. A restart then reads as a step back of more than half the 32-bit
        // range, like a wrap; only the time that passed tells them apart (a wrap here
        // would mean 6.8 days of controller time in two minutes).
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
        // Disconnected for a minute across the wrap: the step the wrap implies (61 s)
        // fits the minute that passed.
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
        // The next real stamp is not a 32-bit wrap from the garbled one.
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
        // A different clock (another controller at the address, or a set clock),
        // far more than half a turn ahead of the last stamp.
        let later = t0 + Duration::from_secs(60);
        let (_, t1, e1) = tl.map(3_000_000_000, later);
        assert!(matches!(e1, ClockEvent::Reset { .. }), "{e1:?}");
        assert!(t1 > tw, "the timeline went back: {t1} after {tw}");
        let (_, t2, e2) = tl.map(3_000_000_004, later + Duration::from_millis(4));
        assert_eq!((t2, e2), (t1 + 4, ClockEvent::None), "and it carries on from there, so the store accepts the samples");
    }

    #[test]
    fn the_wall_clock_stays_known_while_reconnecting() {
        // A snapshot saved between a reconnect's TCP connect and its first sample
        // (which can take a minute of starved retries) still maps its rows to the
        // wall clock; the next stamp then refreshes the anchor.
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
        // Streams are a tick out of step (the cell's drive-side ones run one behind),
        // so right after a restart another stream's stamp can be just below the one
        // that showed it.
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
