use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "ABB Signal Spy";

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

pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    portable_dir_of(exe.parent()?)
}

pub fn portable_dir_of(dir: &Path) -> Option<PathBuf> {
    crate::settings::is_ours_file(&dir.join(crate::settings::FILE)).then(|| dir.to_path_buf())
}

pub fn default_record_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| documents().join("TestSignals")).clone()
}

#[cfg(test)]
static DOCUMENTS_LOOKUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(pv: *const std::ffi::c_void);
}

#[cfg(windows)]
fn documents() -> PathBuf {
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
    #[cfg(test)]
    DOCUMENTS_LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

#[cfg(test)]
mod tests {
    use super::*;
    use spy_core::testdir::TestDir;

    #[test]
    fn a_folder_is_taken_for_a_portable_one_only_for_this_programs_settings() {
        let dir = TestDir::new("portable");
        assert_eq!(portable_dir_of(&dir), None, "no settings file");
        std::fs::write(dir.join("settings.json"), r#"{"theme": "dark", "fontSize": 14}"#).unwrap();
        assert_eq!(portable_dir_of(&dir), None, "another program's settings.json, beside a downloaded exe");
        crate::settings::Settings::default().save(&dir.join("settings.json")).unwrap();
        assert_eq!(portable_dir_of(&dir).as_deref(), Some(dir.as_ref()));
    }

    #[test]
    fn the_documents_folder_is_looked_up_once() {
        let a = default_record_dir();
        let before = DOCUMENTS_LOOKUPS.load(std::sync::atomic::Ordering::SeqCst);
        for _ in 0..100 {
            assert_eq!(default_record_dir(), a);
        }
        assert_eq!(DOCUMENTS_LOOKUPS.load(std::sync::atomic::Ordering::SeqCst), before, "a lookup a frame");
    }
}
