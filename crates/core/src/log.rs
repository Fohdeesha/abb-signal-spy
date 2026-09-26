//! The session log: every define and its answer, refusals, reconnects, stale and
//! resumed readings. Kept in memory for the window (bounded) and, when a file is
//! attached, written line by line as it happens.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::SystemTime;

pub const KEEP: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Entry {
    pub seq: u64,
    #[serde(skip)]
    pub wall: SystemTime,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
struct Inner {
    entries: VecDeque<Entry>,
    next: u64,
    file: Option<File>,
}

#[derive(Default)]
pub struct LogBook {
    inner: Mutex<Inner>,
}

impl LogBook {
    pub fn new() -> LogBook {
        LogBook::default()
    }

    /// Mirror every entry to a file from now on (appending).
    pub fn attach_file(&self, file: File) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).file = Some(file);
    }

    pub fn push(&self, level: Level, text: impl Into<String>) {
        let text = text.into();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seq = g.next;
        g.next += 1;
        let wall = SystemTime::now();
        if let Some(f) = g.file.as_mut() {
            let line = format!("{} {:5} {}\n", crate::util::wall_iso(wall), format!("{level:?}").to_uppercase(), text);
            // A log file that stops accepting writes must not take the session with
            // it; the in-memory log carries on.
            if f.write_all(line.as_bytes()).is_err() {
                g.file = None;
            }
        }
        g.entries.push_back(Entry { seq, wall, level, text });
        while g.entries.len() > KEEP {
            g.entries.pop_front();
        }
    }

    pub fn info(&self, text: impl Into<String>) {
        self.push(Level::Info, text)
    }
    pub fn warn(&self, text: impl Into<String>) {
        self.push(Level::Warn, text)
    }
    pub fn error(&self, text: impl Into<String>) {
        self.push(Level::Error, text)
    }

    /// Entries with `seq >= from`, oldest first.
    pub fn since(&self, from: u64) -> Vec<Entry> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.entries.iter().filter(|e| e.seq >= from).cloned().collect()
    }

    pub fn next_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).next
    }
}
