//! A note for when memory runs out. When an allocation is refused, Rust stops the
//! program at once, without the panic hook (so no crash.txt) and without a line in the
//! log: on the cell (2026-09-29, tunemaster-testsignals.md s26 item 13) the window
//! simply vanished, the PC at its commit limit. The global allocator here passes every
//! request to the system's and, when one is refused, writes a note to crash.txt and a
//! line to signal-spy.log first.
//!
//! Nothing on that path allocates: the files' paths are prepared at startup, the text
//! is put together on the stack, and the files are written with Win32 directly. A
//! refusal does not always stop the program (some code asks for memory it can do
//! without), so the note says what happened, not that the program stopped.

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// The system's allocator, noting a refusal first.
pub struct NoteOnRefusal<A: GlobalAlloc>(pub A);

unsafe impl<A: GlobalAlloc> GlobalAlloc for NoteOnRefusal<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract, passed on unchanged.
        let p = unsafe { self.0.alloc(layout) };
        if p.is_null() {
            note(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { self.0.alloc_zeroed(layout) };
        if p.is_null() {
            note(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { self.0.realloc(ptr, layout, new_size) };
        if p.is_null() {
            note(new_size);
        }
        p
    }
}

/// crash.txt and signal-spy.log in the data folder, as NUL-terminated wide strings.
static PATHS: OnceLock<(Vec<u16>, Vec<u16>)> = OnceLock::new();
/// One note per run: refusals come in bursts when memory runs out.
static NOTED: AtomicBool = AtomicBool::new(false);
/// How many requests were refused (a test reads it).
static REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// Where the note goes; called at startup, while allocating is still possible.
pub fn prepare(dir: &std::path::Path) {
    let _ = PATHS.set((wide(&dir.join("crash.txt")), wide(&dir.join("signal-spy.log"))));
}

fn wide(p: &std::path::Path) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }
    #[cfg(not(windows))]
    {
        p.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect()
    }
}

/// A fixed buffer written into without allocating.
pub struct Text {
    buf: [u8; 640],
    len: usize,
}

impl Text {
    pub fn new() -> Text {
        Text { buf: [0; 640], len: 0 }
    }

    pub fn push(&mut self, s: &str) -> &mut Text {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        self
    }

    /// A number, with at least `width` digits.
    pub fn num(&mut self, mut v: u64, width: usize) -> &mut Text {
        let mut digits = [0u8; 20];
        let mut n = 0;
        while v > 0 || n < width.max(1) {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
        }
        for i in (0..n).rev() {
            let d = [digits[i]];
            // Always ASCII.
            self.push(std::str::from_utf8(&d).unwrap_or("?"));
        }
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Default for Text {
    fn default() -> Text {
        Text::new()
    }
}

/// UTC as the log writes it, `2026-09-29T04:31:27.123Z`.
pub struct Utc {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub ms: u16,
}

fn stamp(t: &mut Text, u: &Utc) {
    t.num(u.year.into(), 4).push("-").num(u.month.into(), 2).push("-").num(u.day.into(), 2).push("T");
    t.num(u.hour.into(), 2).push(":").num(u.minute.into(), 2).push(":").num(u.second.into(), 2).push(".").num(u.ms.into(), 3).push("Z");
}

/// The note in crash.txt.
pub fn crash_note(t: &mut Text, size: usize, at: &Utc) {
    t.push("ABB Signal Spy ").push(env!("CARGO_PKG_VERSION")).push(": at ");
    stamp(t, at);
    t.push(" (UTC) Windows refused it ").num(size as u64, 1).push(" bytes of memory.\r\n\r\n");
    t.push("If the program stopped then, this is why. Usually the PC's memory is used up (its commit limit reached): ");
    t.push("other programs hold it. Close some, then start ABB Signal Spy again. A recording that was running is kept ");
    t.push("up to its last whole row, and opens for review. (A request of many gigabytes would be a defect in the ");
    t.push("program instead: please report it.)\r\n");
}

/// The line in signal-spy.log.
pub fn log_line(t: &mut Text, size: usize, at: &Utc) {
    stamp(t, at);
    t.push(" ERROR Windows refused ").num(size as u64, 1).push(" bytes of memory (the PC's memory used up?). If the program stopped now, this is why; details in crash.txt.\n");
}

fn note(size: usize) {
    REFUSALS.fetch_add(1, Ordering::SeqCst);
    if NOTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some((crash, log)) = PATHS.get() else { return };
    let at = now();
    let mut t = Text::new();
    crash_note(&mut t, size, &at);
    write(crash, t.bytes(), false);
    let mut t = Text::new();
    log_line(&mut t, size, &at);
    write(log, t.bytes(), true);
}

#[cfg(windows)]
fn now() -> Utc {
    use windows_sys::Win32::System::SystemInformation::GetSystemTime;
    // SAFETY: GetSystemTime fills the structure it is given.
    let s = unsafe {
        let mut s = std::mem::zeroed();
        GetSystemTime(&mut s);
        s
    };
    Utc { year: s.wYear, month: s.wMonth, day: s.wDay, hour: s.wHour, minute: s.wMinute, second: s.wSecond, ms: s.wMilliseconds }
}

#[cfg(not(windows))]
fn now() -> Utc {
    Utc { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0, ms: 0 }
}

/// Write `bytes` to the file at `path` (NUL-terminated, wide): replacing it, or at its
/// end. Failures are ignored: there is nothing left to tell them to.
#[cfg(windows)]
fn write(path: &[u16], bytes: &[u8], append: bool) {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, WriteFile, CREATE_ALWAYS, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS};
    let (access, disposition) = if append { (FILE_APPEND_DATA, OPEN_ALWAYS) } else { (GENERIC_WRITE, CREATE_ALWAYS) };
    // SAFETY: `path` is NUL-terminated; the handle is checked and closed.
    unsafe {
        let h = CreateFileW(path.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), disposition, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        let mut written = 0u32;
        WriteFile(h, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null_mut());
        CloseHandle(h);
    }
}

#[cfg(not(windows))]
fn write(_path: &[u16], _bytes: &[u8], _append: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> Utc {
        Utc { year: 2026, month: 9, day: 29, hour: 4, minute: 31, second: 27, ms: 5 }
    }

    #[test]
    fn the_note_says_what_happened_and_what_to_do() {
        let mut t = Text::new();
        crash_note(&mut t, 477_024, &at());
        let s = std::str::from_utf8(t.bytes()).unwrap();
        assert!(s.contains("at 2026-09-29T04:31:27.005Z (UTC) Windows refused it 477024 bytes of memory."), "{s}");
        assert!(s.contains("Close some, then start ABB Signal Spy again") && s.ends_with("report it.)\r\n"), "cut short: {s}");
        let mut t = Text::new();
        log_line(&mut t, 0, &at());
        let s = std::str::from_utf8(t.bytes()).unwrap();
        assert!(s.starts_with("2026-09-29T04:31:27.005Z ERROR Windows refused 0 bytes") && s.ends_with("crash.txt.\n"), "{s}");
    }

    /// An allocator that refuses everything.
    struct Refuses;

    unsafe impl GlobalAlloc for Refuses {
        unsafe fn alloc(&self, _: Layout) -> *mut u8 {
            std::ptr::null_mut()
        }
        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
    }

    #[test]
    fn every_kind_of_refused_request_is_noted() {
        let a = NoteOnRefusal(Refuses);
        let l = Layout::from_size_align(64, 8).unwrap();
        let before = REFUSALS.load(Ordering::SeqCst);
        // SAFETY: nothing is done with the (null) results; `realloc`'s pointer is never
        // read by an allocator that refuses everything.
        unsafe {
            assert!(a.alloc(l).is_null());
            assert!(a.alloc_zeroed(l).is_null());
            let mut x = 0u64;
            assert!(a.realloc(std::ptr::addr_of_mut!(x).cast(), l, 128).is_null());
        }
        // Other tests' requests go to the system's allocator, not this one, so the count
        // is this test's alone.
        assert_eq!(REFUSALS.load(Ordering::SeqCst) - before, 3, "a refused request went unnoted");
    }

    #[test]
    fn numbers_and_a_full_buffer() {
        let mut t = Text::new();
        t.num(0, 1).push(" ").num(7, 3).push(" ").num(u64::MAX, 1);
        assert_eq!(t.bytes(), b"0 007 18446744073709551615");
        let mut t = Text::new();
        for _ in 0..100 {
            t.push("0123456789");
        }
        assert_eq!(t.bytes().len(), 640, "stops at its end instead of overrunning");
    }
}
