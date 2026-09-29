//! The channel store: the live history of every channel, written by the session
//! worker and read by the window, the phone view and "save the last N seconds".
//!
//! One bounded ring per channel of (timeline ms, value). The ring is sized from the
//! channel's sample time, so ten minutes of a 4 ms signal is 150,000 points and ten
//! minutes of a 24 ms one 25,000. Each channel has its own lock, held for one frame's
//! samples by the writer and for one draw's worth of reading by a reader, so neither
//! side waits on the others for long.
//!
//! Charts never draw more points than the screen has pixel columns:
//! [`Ring::decimate`] gives each column the minimum and maximum of the samples it
//! covers, and breaks the line wherever the stamps show a gap, never interpolating
//! across one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use crate::request::{Axis, MechUnit};
use crate::sample::ValueKind;

/// History kept per channel (ten minutes).
pub const HISTORY_S: f64 = 600.0;
/// The most points one ring holds, whatever its sample time says: ten minutes at the
/// fastest rate anything has been seen streaming, with headroom.
pub const MAX_POINTS: usize = 200_000;

/// What identifies a channel: the stream definition. The same signal on another
/// unit or axis is another channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ChannelKey {
    pub signal: u32,
    pub unit: MechUnit,
    pub axis: Axis,
}

impl ChannelKey {
    /// Stable text id, used in recordings: `4002/ROB_1/J2` (signal / mechanical unit /
    /// joint, counted from 1). No commas, quotes or spaces, so it needs no quoting in
    /// a CSV. A signal that is not per joint still names the axis it was asked for.
    pub fn id(&self) -> String {
        format!("{}/{}/J{}", self.signal, self.unit, self.axis.one_based())
    }

    /// Reads [`ChannelKey::id`], and the bare axis number (`4002/ROB_1/2`) that
    /// recordings made before 2026-09-27 wrote.
    pub fn parse_id(s: &str) -> Option<ChannelKey> {
        let mut it = s.split('/');
        let signal = it.next()?.parse().ok()?;
        let unit = MechUnit::new(it.next()?).ok()?;
        let a = it.next()?;
        let a = a.strip_prefix('J').unwrap_or(a);
        // Digits only: `parse` would take a sign ("J+2").
        if a.is_empty() || !a.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let axis = Axis::new(a.parse().ok()?)?;
        if it.next().is_some() {
            return None;
        }
        Some(ChannelKey { signal, unit, axis })
    }
}

impl std::fmt::Display for ChannelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} axis {}", self.signal, self.unit, self.axis.one_based())
    }
}

/// One channel's history.
#[derive(Debug, Clone)]
pub struct Ring {
    t: VecDeque<i64>,
    v: VecDeque<f64>,
    cap: usize,
    /// Nominal sample spacing in ms; a step beyond 1.5 of it is a gap.
    pub sample_ms: f64,
    pub kind: Option<ValueKind>,
    /// Samples refused for going backwards in time within the channel.
    pub out_of_order: u64,
    /// Newest string sample, for string-typed signals (not charted).
    pub last_text: Option<String>,
}

impl Ring {
    pub fn new(sample_ms: f64) -> Ring {
        let sample_ms = if sample_ms.is_finite() && sample_ms > 0.0 { sample_ms } else { 4.0 };
        let cap = ((HISTORY_S * 1000.0 / sample_ms).ceil() as usize + 16).min(MAX_POINTS);
        Ring { t: VecDeque::with_capacity(cap.min(4096)), v: VecDeque::with_capacity(cap.min(4096)), cap, sample_ms, kind: None, out_of_order: 0, last_text: None }
    }

    pub fn len(&self) -> usize {
        self.t.len()
    }
    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Append one sample. A stamp earlier than the newest is refused (and counted):
    /// within one stream the controller's stamps only move forward, and a history
    /// out of order would break every search below.
    pub fn push(&mut self, t: i64, v: f64) -> bool {
        if let Some(&last) = self.t.back()
            && t < last {
                self.out_of_order += 1;
                return false;
            }
        if self.t.len() == self.cap {
            self.t.pop_front();
            self.v.pop_front();
        }
        self.t.push_back(t);
        self.v.push_back(v);
        true
    }

    pub fn clear(&mut self) {
        self.t.clear();
        self.v.clear();
    }

    pub fn first_t(&self) -> Option<i64> {
        self.t.front().copied()
    }
    pub fn last(&self) -> Option<(i64, f64)> {
        Some((*self.t.back()?, *self.v.back()?))
    }

    /// Index of the first sample at or after `t`.
    pub fn lower_bound(&self, t: i64) -> usize {
        self.t.partition_point(|&x| x < t)
    }

    /// The newest sample at or before `t`, found by search (not by walking the ring).
    pub fn at_or_before(&self, t: i64) -> Option<(i64, f64)> {
        let i = self.lower_bound(t.saturating_add(1));
        (i > 0).then(|| (self.t[i - 1], self.v[i - 1]))
    }

    /// Samples with `from <= t < to`, in order.
    pub fn range(&self, from: i64, to: i64) -> impl Iterator<Item = (i64, f64)> + '_ {
        let a = self.lower_bound(from);
        let b = self.lower_bound(to);
        (a..b).map(move |i| (self.t[i], self.v[i]))
    }

    /// The gap threshold for this channel.
    pub fn gap_ms(&self) -> f64 {
        // 1.5 sample times: a step of 4 or 5 on the IRC5's 4.032 ms tick is normal,
        // a missed sample (8) is a gap.
        (self.sample_ms * 1.5).max(1.0)
    }

    /// Mean of the samples in the last `window_ms` before the newest sample (the
    /// numeric readouts show a 150 ms mean; the charts stay raw). NaN samples are
    /// skipped; `None` when nothing finite is in the window.
    pub fn recent_mean(&self, window_ms: i64) -> Option<f64> {
        let (last_t, _) = self.last()?;
        let (mut sum, mut n) = (0.0, 0u32);
        for i in (self.lower_bound(last_t - window_ms)..self.t.len()).rev() {
            let v = self.v[i];
            if v.is_finite() {
                sum += v;
                n += 1;
            }
        }
        (n > 0).then(|| sum / f64::from(n))
    }

    /// Per-pixel-column decimation over `[from, to)` into `columns` buckets. Each
    /// column carries the first, minimum, maximum and last value of the samples it
    /// covers, so the drawn trace keeps every spike however long the window. A step
    /// between samples longer than the gap threshold, or a NaN, ends a segment: the
    /// chart draws a gap as a gap and never interpolates across it.
    ///
    /// `transform` sees the samples in time order, so it may carry state (the
    /// "hold last non-zero" display of a zero-filled signal does).
    pub fn decimate(&self, from: i64, to: i64, columns: usize, mut transform: impl FnMut(f64) -> f64) -> Vec<Vec<Column>> {
        self.decimate_at(from, to, columns, |_, v| transform(v))
    }

    /// [`Ring::decimate`], the transform given each sample's time too (a zero-filled
    /// signal's hold is by time).
    pub fn decimate_at(&self, from: i64, to: i64, columns: usize, mut transform: impl FnMut(i64, f64) -> f64) -> Vec<Vec<Column>> {
        let mut segments: Vec<Vec<Column>> = Vec::new();
        if columns == 0 || to <= from {
            return segments;
        }
        let span = (to - from) as f64;
        let gap = self.gap_ms();
        let mut current: Vec<Column> = Vec::new();
        let mut col_idx: Option<usize> = None;
        let mut prev_t: Option<i64> = None;
        // Start one sample early so a line enters the window from its left edge.
        let start = self.lower_bound(from).saturating_sub(1);
        let end = (self.lower_bound(to) + 1).min(self.t.len());
        for i in start..end {
            let (t, raw) = (self.t[i], self.v[i]);
            let v = transform(t, raw);
            if let Some(p) = prev_t
                && (t - p) as f64 > gap && !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                    col_idx = None;
                }
            prev_t = Some(t);
            if !v.is_finite() {
                // A NaN sample is a hole in the data, drawn as one.
                if !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
                col_idx = None;
                continue;
            }
            let c = (((t - from) as f64 / span) * columns as f64).floor().clamp(-1.0, columns as f64) as i64;
            let c = c.max(0) as usize;
            if col_idx == Some(c) {
                let last = current.last_mut().unwrap();
                last.min = last.min.min(v);
                last.max = last.max.max(v);
                last.last_v = v;
            } else {
                current.push(Column { t, min: v, max: v, first_v: v, last_v: v });
                col_idx = Some(c);
            }
        }
        if !current.is_empty() {
            segments.push(current);
        }
        segments
    }
}

/// One pixel column of a decimated trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub t: i64,
    pub min: f64,
    pub max: f64,
    pub first_v: f64,
    pub last_v: f64,
}

/// One channel in the store.
#[derive(Debug)]
pub struct Channel {
    pub key: ChannelKey,
    ring: Mutex<Ring>,
}

impl Channel {
    pub fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Every channel's history. Channels are created when first defined and kept (with
/// their history) until removed by the user, so a channel that drops out during a
/// reconnect keeps what it had.
#[derive(Debug, Default)]
pub struct Store {
    channels: RwLock<Vec<Arc<Channel>>>,
    /// Counts [`Store::clear`]s: a reader keeping anything derived from the history
    /// (statistics since a reset, markers on its clock) starts afresh when it moves.
    epoch: std::sync::atomic::AtomicU64,
}

impl Store {
    pub fn new() -> Store {
        Store::default()
    }

    /// The channel for `key`, created with this sample time if new. An existing
    /// channel whose sample time turns out different (the controller's reply is the
    /// authority) is re-sized, keeping its history.
    pub fn channel(&self, key: &ChannelKey, sample_ms: f64) -> Arc<Channel> {
        if let Some(c) = self.get(key) {
            let mut r = c.lock();
            if (r.sample_ms - sample_ms).abs() > 1e-6 && sample_ms.is_finite() && sample_ms > 0.0 {
                let mut fresh = Ring::new(sample_ms);
                fresh.kind = r.kind;
                for i in 0..r.t.len() {
                    fresh.push(r.t[i], r.v[i]);
                }
                *r = fresh;
            }
            drop(r);
            return c;
        }
        let mut w = self.channels.write().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = w.iter().find(|c| &c.key == key) {
            return c.clone();
        }
        let c = Arc::new(Channel { key: key.clone(), ring: Mutex::new(Ring::new(sample_ms)) });
        w.push(c.clone());
        c
    }

    pub fn get(&self, key: &ChannelKey) -> Option<Arc<Channel>> {
        self.channels.read().unwrap_or_else(|e| e.into_inner()).iter().find(|c| &c.key == key).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Channel>> {
        self.channels.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn remove(&self, key: &ChannelKey) {
        self.channels.write().unwrap_or_else(|e| e.into_inner()).retain(|c| &c.key != key);
    }

    /// Forget every channel's history (another controller: its clock and values
    /// have nothing to do with the last one's).
    pub fn clear(&self) {
        self.channels.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Changes whenever the history is cleared.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Newest timeline ms across all channels.
    pub fn newest(&self) -> Option<i64> {
        self.all().iter().filter_map(|c| c.lock().last().map(|(t, _)| t)).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ChannelKey {
        ChannelKey { signal: 4002, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(2).unwrap() }
    }

    #[test]
    fn key_ids_round_trip() {
        let k = key();
        // The joint spelled out, so nobody reads the axis as zero-based (the wire's is).
        assert_eq!(k.id(), "4002/ROB_1/J2");
        assert_eq!(ChannelKey::parse_id(&k.id()), Some(k.clone()));
        // Recordings made before 2026-09-27 (the cell's among them) wrote the bare number.
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/2"), Some(k));
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/J0"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/JJ2"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/J"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/0"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/J+2"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/+2"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB 1/1"), None);
        assert_eq!(ChannelKey::parse_id("4002/ROB_1/1/9"), None);
    }

    #[test]
    fn ring_is_bounded_and_ordered() {
        let mut r = Ring::new(4.0);
        assert_eq!(r.capacity(), 150_016);
        let mut small = Ring { cap: 3, ..Ring::new(4.0) };
        for t in 0..5 {
            assert!(small.push(t * 4, t as f64));
        }
        assert_eq!(small.len(), 3);
        assert_eq!(small.first_t(), Some(8));
        assert!(!small.push(0, 9.0), "a stamp from the past is refused");
        assert_eq!(small.out_of_order, 1);
        assert!(small.push(16, 1.0), "an equal stamp is accepted");
        r.push(0, 1.0);
        assert_eq!(r.range(0, 1).count(), 1);
    }

    #[test]
    fn mean_skips_nan_and_is_windowed() {
        let mut r = Ring::new(4.0);
        for i in 0..100 {
            r.push(i * 4, if i == 99 { f64::NAN } else if i >= 60 { 10.0 } else { 0.0 });
        }
        // Last 150 ms: stamps 246..396, i.e. i = 62..99, all 10 except the NaN.
        assert_eq!(r.recent_mean(150), Some(10.0));
        let mut empty = Ring::new(4.0);
        assert_eq!(empty.recent_mean(150), None);
        empty.push(0, f64::NAN);
        assert_eq!(empty.recent_mean(150), None);
    }

    #[test]
    fn decimation_is_bounded_and_breaks_at_gaps() {
        let mut r = Ring::new(4.0);
        for i in 0..10_000i64 {
            // A gap from 20 s to 21 s.
            let t = i * 4;
            if (20_000..21_000).contains(&t) {
                continue;
            }
            r.push(t, (i % 2) as f64);
        }
        let segs = r.decimate(0, 40_000, 800, |v| v);
        assert_eq!(segs.len(), 2, "the gap must split the trace");
        let cols: usize = segs.iter().map(|s| s.len()).sum();
        assert!(cols <= 802, "{cols} columns for 800 pixels");
        // Every column spans the square wave's full swing.
        assert!(segs[0][10].min == 0.0 && segs[0][10].max == 1.0);
        // A NaN is a hole.
        let mut n = Ring::new(4.0);
        for i in 0..10 {
            n.push(i * 4, if i == 5 { f64::NAN } else { 1.0 });
        }
        assert_eq!(n.decimate(0, 40, 40, |v| v).len(), 2);
        // Degrees transform applied.
        let d = n.decimate(0, 20, 5, |v| v * 2.0);
        assert_eq!(d[0][0].max, 2.0);
    }

    #[test]
    fn a_missed_sample_is_a_gap_but_the_irc5_tick_is_not() {
        let mut r = Ring::new(4.032);
        // Steps of 4 with a 5 every so often: normal.
        let mut t = 0;
        for i in 0..100 {
            r.push(t, 1.0);
            t += if i % 31 == 30 { 5 } else { 4 };
        }
        assert_eq!(r.decimate(0, t, 1000, |v| v).len(), 1);
        r.push(t + 4, 1.0); // a missed sample: step 8
        assert_eq!(r.decimate(0, t + 10, 1000, |v| v).len(), 2);
    }

    #[test]
    fn store_keeps_history_across_a_resize() {
        let s = Store::new();
        let c = s.channel(&key(), 4.032);
        c.lock().push(1, 2.0);
        let c2 = s.channel(&key(), 24.192);
        assert!(Arc::ptr_eq(&c, &c2));
        assert_eq!(c2.lock().len(), 1);
        assert_eq!(c2.lock().sample_ms, 24.192);
        assert_eq!(s.all().len(), 1);
        s.remove(&key());
        assert!(s.get(&key()).is_none());
    }
}
