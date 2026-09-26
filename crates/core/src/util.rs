//! Small helpers with no better home.

use std::time::{SystemTime, UNIX_EPOCH};

/// UTC wall time as ISO 8601 with milliseconds, without pulling in a date crate:
/// days-from-civil arithmetic (Howard Hinnant's algorithm, public domain).
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

/// Text without a leading UTF-8 byte-order mark. Notepad and PowerShell 5 write one,
/// and a JSON parser refuses it; a person who hand-edits a settings or catalogue
/// file must not have it rejected for that.
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

/// UTC date and time for folder names: `2026-09-25_14-03-07`.
pub fn wall_stamp(t: SystemTime) -> String {
    let iso = wall_iso(t);
    format!("{}_{}", &iso[0..10], iso[11..19].replace(':', "-"))
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
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
}
