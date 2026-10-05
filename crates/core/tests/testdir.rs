//! A test process starts clean: its first test folder sweeps away the ones a killed run
//! left over a day ago, and a folder of the same name left by a killed run of a process
//! that had the same id is emptied first. One test in its own process, so its folder is
//! the process's first.

#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, SystemTime};

use spy_core::testdir::TestDir;

/// Set a folder's last-changed time back by `by`.
fn age_by(dir: &Path, by: Duration) {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS to open a folder at all.
    let f = std::fs::OpenOptions::new().access_mode(0x100).custom_flags(0x0200_0000).open(dir).unwrap();
    f.set_modified(SystemTime::now() - by).unwrap();
}

#[test]
fn the_first_folder_sweeps_a_killed_runs_leftovers() {
    let temp = std::env::temp_dir();
    let killed = temp.join(format!("spy-test-killed-{}-x", std::process::id()));
    std::fs::create_dir_all(killed.join("recordings")).unwrap();
    age_by(&killed, Duration::from_secs(2 * 24 * 3600));
    let recent = temp.join(format!("spy-test-recent-{}-x", std::process::id()));
    std::fs::create_dir_all(&recent).unwrap();
    age_by(&recent, Duration::from_secs(3600));
    // What a killed run of a process with this id left under the name about to be used.
    let reused = temp.join(format!("spy-test-reused-{}-0", std::process::id()));
    std::fs::create_dir_all(&reused).unwrap();
    std::fs::write(reused.join("settings.json"), "{\"dark\": false}").unwrap();

    let d = TestDir::new("reused");

    assert!(!killed.exists(), "a two-day-old test folder was left");
    assert!(recent.is_dir(), "a folder of an hour ago was swept: a run may still use it");
    let _ = std::fs::remove_dir_all(&recent);
    assert_eq!(d.path(), reused, "not the process's first folder");
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "the killed run's files are still in it");
}
