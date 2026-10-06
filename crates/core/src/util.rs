use std::time::{SystemTime, UNIX_EPOCH};

pub fn wall_iso(t: SystemTime) -> String {
    let (secs, millis) = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_millis()),
        Err(e) => {
            let d = e.duration();
            let s = -(d.as_secs() as i64) - i64::from(d.subsec_nanos() > 0);
            (s, if d.subsec_nanos() > 0 { 1000 - d.subsec_millis() } else { 0 })
        }
    };
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", sod / 3600, (sod / 60) % 60, sod % 60)
}

pub fn parse_iso(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || *b.last()? != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if t.bytes().all(|c| c.is_ascii_digit()) { t.parse().ok() } else { None }
    };
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let frac = &s[19..s.len() - 1];
    let nanos = match frac.strip_prefix('.') {
        None if frac.is_empty() => 0,
        Some(f) if !f.is_empty() && f.len() <= 9 && f.bytes().all(|c| c.is_ascii_digit()) => f.parse::<u32>().ok()? * 10u32.pow(9 - f.len() as u32),
        _ => return None,
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    if civil_from_days(days) != (y, mo as u32, d as u32) {
        return None;
    }
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec;
    let d = std::time::Duration::new(secs.unsigned_abs(), nanos);
    if secs >= 0 { UNIX_EPOCH.checked_add(d) } else { UNIX_EPOCH.checked_sub(std::time::Duration::from_secs(secs.unsigned_abs()))?.checked_add(std::time::Duration::new(0, nanos)) }
}

pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

pub fn wall_stamp(t: SystemTime) -> String {
    let iso = wall_iso(t);
    format!("{}_{}", &iso[0..10], iso[11..19].replace(':', "-"))
}

pub fn local_parts(t: SystemTime) -> Option<(u16, u16, u16, u16, u16, u16, u16)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        let d = t.duration_since(UNIX_EPOCH).ok()?;
        let ticks = d.as_nanos() / 100 + 116_444_736_000_000_000;
        let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        unsafe {
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut local: SYSTEMTIME = std::mem::zeroed();
            if FileTimeToSystemTime(&ft, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0 {
                return Some((local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond, local.wMilliseconds));
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = t;
        None
    }
}

pub fn local_stamp(t: SystemTime) -> String {
    match local_parts(t) {
        Some((y, mo, d, h, mi, s, _)) => format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}"),
        None => format!("{}Z", wall_stamp(t)),
    }
}

pub fn free_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        unsafe {
            let mut m: MEMORYSTATUSEX = std::mem::zeroed();
            m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut m) != 0).then_some(m.ullAvailPageFile)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn write_whole(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {e}", tmp.display()));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_whole_file_is_written_in_one_go_beside_itself() {
        let dir = crate::testdir::TestDir::new("write-whole");
        let p = dir.join("settings.json");
        super::write_whole(&p, b"one").unwrap();
        super::write_whole(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let names: Vec<String> = std::fs::read_dir(&*dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["settings.json"], "a temporary file left behind");
        std::fs::create_dir(dir.join("taken")).unwrap();
        assert!(super::write_whole(&dir.join("taken"), b"x").is_err());
        let names = std::fs::read_dir(&*dir).unwrap().count();
        assert_eq!(names, 2, "a failed write leaves its temporary file");
    }

    #[test]
    fn the_memory_free_is_known_on_windows() {
        if cfg!(windows) {
            assert!(super::free_memory().is_some_and(|b| b > 0));
        }
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn iso_dates() {
        assert_eq!(wall_iso(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(wall_iso(UNIX_EPOCH + Duration::from_millis(951_782_400_123)), "2000-02-29T00:00:00.123Z");
        assert_eq!(wall_iso(UNIX_EPOCH + Duration::from_secs(1_790_346_187)), "2026-09-25T14:23:07.000Z");
        assert_eq!(wall_stamp(UNIX_EPOCH + Duration::from_secs(1_790_346_187)), "2026-09-25_14-23-07");
        assert_eq!(wall_iso(UNIX_EPOCH - Duration::from_millis(1)), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn iso_dates_read_back() {
        for ms in [0i64, 951_782_400_123, 1_790_346_187_000, 1_790_412_440_523, -1, -86_400_001, 4_102_444_800_999] {
            let t = if ms >= 0 { UNIX_EPOCH + Duration::from_millis(ms as u64) } else { UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs()) };
            assert_eq!(parse_iso(&wall_iso(t)), Some(t), "{ms}: {}", wall_iso(t));
        }
        assert_eq!(parse_iso("2026-09-26T08:47:20Z"), Some(UNIX_EPOCH + Duration::from_secs(1_790_412_440)));
        assert_eq!(parse_iso("2026-09-26T08:47:20.5Z"), Some(UNIX_EPOCH + Duration::from_millis(1_790_412_440_500)));
        for bad in ["", "2026-09-26", "2026-09-26T08:47:20.523", "2026-02-30T00:00:00Z", "2026-13-01T00:00:00Z", "2026-09-26T24:00:00Z", "2026-09-26T08:47:20.Z", "2026-09-26 08:47:20Z", "+026-09-26T08:47:20Z", "2026-09-26T08:47:20.1234567890Z"] {
            assert_eq!(parse_iso(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_stamps_are_file_name_safe() {
        let t = UNIX_EPOCH + Duration::from_secs(1_790_346_187);
        let s = local_stamp(t);
        assert_eq!(s.len(), if cfg!(windows) { 19 } else { 20 }, "{s}");
        assert!(s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '_' || c == 'Z'), "{s}");
        #[cfg(windows)]
        {
            let (y, mo, d, h, mi, sec, _) = local_parts(t).expect("Windows gives local time");
            assert_eq!(s, format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{sec:02}"));
        }
    }
}
