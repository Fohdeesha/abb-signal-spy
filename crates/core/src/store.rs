use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use crate::request::{Axis, MechUnit};
use crate::sample::ValueKind;

pub const HISTORY_S: f64 = 600.0;
pub const MAX_POINTS: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ChannelKey {
    pub signal: u32,
    pub unit: MechUnit,
    pub axis: Axis,
}

impl ChannelKey {
    pub fn id(&self) -> String {
        format!("{}/{}/J{}", self.signal, self.unit, self.axis.one_based())
    }

    pub fn parse_id(s: &str) -> Option<ChannelKey> {
        let mut it = s.split('/');
        let signal = it.next()?.parse().ok()?;
        let unit = MechUnit::new(it.next()?).ok()?;
        let a = it.next()?;
        let a = a.strip_prefix('J').unwrap_or(a);
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

#[derive(Debug, Clone)]
pub struct Ring {
    t: VecDeque<i64>,
    v: VecDeque<f64>,
    cap: usize,
    pub sample_ms: f64,
    pub kind: Option<ValueKind>,
    pub out_of_order: u64,
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

    pub fn lower_bound(&self, t: i64) -> usize {
        self.t.partition_point(|&x| x < t)
    }

    pub fn at_or_before(&self, t: i64) -> Option<(i64, f64)> {
        let i = self.lower_bound(t.saturating_add(1));
        (i > 0).then(|| (self.t[i - 1], self.v[i - 1]))
    }

    pub fn range(&self, from: i64, to: i64) -> impl Iterator<Item = (i64, f64)> + '_ {
        let a = self.lower_bound(from);
        let b = self.lower_bound(to);
        (a..b).map(move |i| (self.t[i], self.v[i]))
    }

    pub fn gap_ms(&self) -> f64 {
        (self.sample_ms * 1.5).max(1.0)
    }

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

    pub fn decimate(&self, from: i64, to: i64, columns: usize, mut transform: impl FnMut(f64) -> f64) -> Vec<Vec<Column>> {
        self.decimate_at(from, to, columns, |_, v| transform(v))
    }

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub t: i64,
    pub min: f64,
    pub max: f64,
    pub first_v: f64,
    pub last_v: f64,
}

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

#[derive(Debug, Default)]
pub struct Store {
    channels: RwLock<Vec<Arc<Channel>>>,
    epoch: std::sync::atomic::AtomicU64,
}

impl Store {
    pub fn new() -> Store {
        Store::default()
    }

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

    pub fn clear(&self) {
        self.channels.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

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
        assert_eq!(k.id(), "4002/ROB_1/J2");
        assert_eq!(ChannelKey::parse_id(&k.id()), Some(k.clone()));
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
        assert!(segs[0][10].min == 0.0 && segs[0][10].max == 1.0);
        let mut n = Ring::new(4.0);
        for i in 0..10 {
            n.push(i * 4, if i == 5 { f64::NAN } else { 1.0 });
        }
        assert_eq!(n.decimate(0, 40, 40, |v| v).len(), 2);
        let d = n.decimate(0, 20, 5, |v| v * 2.0);
        assert_eq!(d[0][0].max, 2.0);
    }

    #[test]
    fn a_missed_sample_is_a_gap_but_the_irc5_tick_is_not() {
        let mut r = Ring::new(4.032);
        let mut t = 0;
        for i in 0..100 {
            r.push(t, 1.0);
            t += if i % 31 == 30 { 5 } else { 4 };
        }
        assert_eq!(r.decimate(0, t, 1000, |v| v).len(), 1);
        r.push(t + 4, 1.0);
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
