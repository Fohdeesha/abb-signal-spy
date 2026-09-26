//! Where things live on disk.
//!
//! Settings and the log go in `%LOCALAPPDATA%\ABB Signal Spy`, or beside the `.exe`
//! when a `settings.json` is already there (a portable copy on a USB stick keeps its
//! settings with it, E5). Recordings default to `Documents\TestSignals` (C3).

use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "ABB Signal Spy";

/// The folder for settings and the log, created if needed. `ABB_SIGNAL_SPY_DATA`
/// overrides it (a second, separate set of settings, or a test run).
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ABB_SIGNAL_SPY_DATA").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(d);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    if let Some(dir) = portable_dir() {
        return dir;
    }
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).or_else(|| std::env::var_os("APPDATA").map(PathBuf::from)).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(APP_DIR);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The `.exe`'s folder, if a settings file is already there.
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    dir.join("settings.json").is_file().then_some(dir)
}

/// `Documents\TestSignals`.
pub fn default_record_dir() -> PathBuf {
    documents().join("TestSignals")
}

// From ole32.dll, which has it on every Windows: windows-sys names combase.dll, which
// Windows 7 does not have, and an exe importing it does not start there.
#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(pv: *const std::ffi::c_void);
}

#[cfg(windows)]
fn documents() -> PathBuf {
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    // The known-folder call follows a Documents folder redirected elsewhere (to
    // OneDrive, or a network share), which %USERPROFILE%\Documents does not.
    unsafe {
        let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut p);
        if hr >= 0 && !p.is_null() {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            CoTaskMemFree(p.cast_const().cast());
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        } else if !p.is_null() {
            CoTaskMemFree(p.cast_const().cast());
        }
    }
    fallback_documents()
}

#[cfg(not(windows))]
fn documents() -> PathBuf {
    fallback_documents()
}

fn fallback_documents() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| Path::new(&h).join("Documents")).unwrap_or_else(std::env::temp_dir)
}

/// Open a folder in Explorer.
pub fn open_folder(dir: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
