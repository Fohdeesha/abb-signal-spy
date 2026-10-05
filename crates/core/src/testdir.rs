use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const PREFIX: &str = "spy-test-";

const LEFT_OVER: Duration = Duration::from_secs(24 * 3600);

pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        static SWEPT: Once = Once::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        SWEPT.call_once(|| sweep(&base, LEFT_OVER));
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = base.join(format!("{PREFIX}{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        for _ in 0..20 {
            match std::fs::remove_dir_all(&self.0) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(25)),
                _ => return,
            }
        }
    }
}

fn sweep(base: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    let now = SystemTime::now();
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        let changed = e.metadata().and_then(|m| m.modified());
        if changed.is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d > age)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_folder_goes_when_it_is_dropped() {
        let d = TestDir::new("drop");
        std::fs::create_dir_all(d.join("recordings").join("one")).unwrap();
        std::fs::write(d.join("recordings").join("one").join("samples.csv"), "t,v\n").unwrap();
        let p = d.to_path_buf();
        assert!(p.is_dir());
        drop(d);
        assert!(!p.exists(), "{} is still there", p.display());
    }

    #[test]
    fn a_failed_tests_folder_goes_too() {
        let mut p = PathBuf::new();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let d = TestDir::new("failed");
            std::fs::write(d.join("settings.json"), "{}").unwrap();
            p = d.to_path_buf();
            panic!("a test failing on purpose: its folder must still go");
        }));
        assert!(failed.is_err());
        assert!(p.is_absolute(), "the folder was never made");
        assert!(!p.exists(), "{} is still there", p.display());
    }

    #[cfg(windows)]
    #[test]
    fn a_file_held_open_for_a_moment_does_not_keep_the_folder() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = TestDir::new("held");
        let p = d.to_path_buf();
        let f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(d.join("data.csv")).unwrap();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(f);
        });
        drop(d);
        closer.join().unwrap();
        assert!(!p.exists(), "{} is still there", p.display());
    }

    #[test]
    fn two_tests_with_one_tag_never_share_a_folder() {
        let (a, b) = (TestDir::new("same"), TestDir::new("same"));
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && b.is_dir());
    }

    #[test]
    fn a_killed_runs_folders_are_swept_and_nothing_else() {
        let base = TestDir::new("sweep");
        for name in ["spy-test-killed-1-0", "spy-ui-old", "keep-me"] {
            std::fs::create_dir_all(base.join(name).join("recordings")).unwrap();
        }
        std::fs::write(base.join("spy-test-a-file"), "not a folder").unwrap();
        std::thread::sleep(Duration::from_millis(50));

        sweep(&base, Duration::from_secs(3600));
        assert!(base.join("spy-test-killed-1-0").is_dir(), "swept a folder younger than the age");

        sweep(&base, Duration::from_millis(10));
        assert!(!base.join("spy-test-killed-1-0").exists(), "an old test folder was left");
        assert!(base.join("spy-ui-old").is_dir() && base.join("keep-me").is_dir(), "swept a folder of another name");
        assert!(base.join("spy-test-a-file").is_file(), "swept a file");
    }
}
