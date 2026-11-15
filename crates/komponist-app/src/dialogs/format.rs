//! Texts the dialogs compute: local dates ("vor 2 Stunden" / "2 hours ago", "gestern", "12. Okt." /
//! "Oct 12", "heute 09:12"), the latency, paths with `~`. Words and plurals come from the catalog
//! (`dialog.ftl`).
//!
//! The formatting functions take [`LocalTime`] values, so tests give fixed times; [`local`]
//! converts a [`SystemTime`] with the system's time zone (`localtime_r`).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use komponist_i18n::Lang;

/// The abbreviation of month `month` (1 ..= 12) as the screens write it ("Okt."; Duden
/// abbreviations) or as English does ("Oct").
fn month_name(t: Lang, month: u8) -> &'static str {
    match month.clamp(1, 12) {
        1 => t.dialog_month_1(),
        2 => t.dialog_month_2(),
        3 => t.dialog_month_3(),
        4 => t.dialog_month_4(),
        5 => t.dialog_month_5(),
        6 => t.dialog_month_6(),
        7 => t.dialog_month_7(),
        8 => t.dialog_month_8(),
        9 => t.dialog_month_9(),
        10 => t.dialog_month_10(),
        11 => t.dialog_month_11(),
        _ => t.dialog_month_12(),
    }
}

/// A point in time on the local calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// Year (2026).
    pub year: i32,
    /// Month 1 ..= 12.
    pub month: u8,
    /// Day of the month 1 ..= 31.
    pub day: u8,
    /// Hour 0 ..= 23.
    pub hour: u8,
    /// Minute 0 ..= 59.
    pub minute: u8,
    /// Seconds since the Unix epoch (for "vor 2 Stunden").
    pub unix: i64,
}

impl LocalTime {
    /// Days since 1970-01-01 on the local calendar (for "gestern").
    pub fn day_number(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `time` on the local calendar of this machine; `None` before 1970 or if the C library fails.
pub fn local(time: SystemTime) -> Option<LocalTime> {
    let unix = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let unix = i64::try_from(unix).ok()?;
    // `time_t` is narrower than `i64` on some targets.
    #[allow(clippy::useless_conversion)]
    let t: libc::time_t = unix.try_into().ok()?;
    // SAFETY: `localtime_r` writes only into `tm`, which lives on this stack frame, and reads
    // only `t`; a null result means failure and leaves nothing to clean up.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month: u8::try_from(tm.tm_mon + 1).ok()?,
        day: u8::try_from(tm.tm_mday).ok()?,
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
        unix,
    })
}

/// "12. Okt." / "Oct 12" (and the year when it is not `now`'s: "12. Okt. 2025" / "Oct 12, 2025").
fn date(t: Lang, then: &LocalTime, now: &LocalTime) -> String {
    let month = month_name(t, then.month);
    if then.year == now.year {
        t.dialog_date(then.day, month)
    } else {
        t.dialog_date_year(then.day, month, then.year)
    }
}

/// When a project was last saved, as the start screen shows it (S09 `recent`): "gerade eben",
/// "vor 5 Minuten", "vor 2 Stunden" (today), "gestern", "12. Okt." ("5 minutes ago", "2 hours ago",
/// "yesterday", "Oct 12" in English).
pub fn relative_date(t: Lang, then: &LocalTime, now: &LocalTime) -> String {
    let days = now.day_number() - then.day_number();
    if days == 0 {
        let secs = (now.unix - then.unix).max(0);
        let minutes = secs / 60;
        let hours = minutes / 60;
        if minutes < 1 {
            t.dialog_just_now().to_owned()
        } else if hours < 1 {
            t.dialog_minutes_ago(minutes)
        } else {
            t.dialog_hours_ago(hours)
        }
    } else if days == 1 {
        t.dialog_yesterday().to_owned()
    } else {
        date(t, then, now)
    }
}

/// The time of a scan (S07 "letzter Scan heute 09:12"): "heute 09:12", "gestern 09:12",
/// "12. Okt. 09:12" (24 hour clock in every language).
pub fn day_and_time(t: Lang, then: &LocalTime, now: &LocalTime) -> String {
    let day = match now.day_number() - then.day_number() {
        0 => t.dialog_today().to_owned(),
        1 => t.dialog_yesterday().to_owned(),
        _ => date(t, then, now),
    };
    t.dialog_day_time(day, format!("{:02}:{:02}", then.hour, then.minute))
}

/// `path` with the home folder written as `~` ("~/Musik/klang/Night Drive.komponist").
pub fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    tilde_in(path, home.as_deref())
}

/// [`tilde`] for a given home folder.
pub fn tilde_in(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Latency text of the S07 buffer row: `buffer / rate` plus what the stream reports, "5.3 ms
/// Latenz" ("5.3 ms latency").
pub fn latency(t: Lang, buffer_frames: u32, sample_rate: u32, reported: Duration) -> String {
    let ms = f64::from(buffer_frames) * 1000.0 / f64::from(sample_rate.max(1))
        + reported.as_secs_f64() * 1000.0;
    t.dialog_latency(format!("{ms:.1}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalTime {
        let unix = days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60;
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            unix,
        }
    }

    #[test]
    fn day_numbers_follow_the_calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(
            days_from_civil(2026, 3, 1) - days_from_civil(2026, 2, 28),
            1
        );
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28),
            2
        );
    }

    #[test]
    fn recent_dates_read_like_s09() {
        let now = at(2026, 11, 14, 16, 30);
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 14, 16, 30), &now),
            "gerade eben"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 14, 16, 29), &now),
            "vor 1 Minute"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 14, 16, 5), &now),
            "vor 25 Minuten"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 14, 14, 20), &now),
            "vor 2 Stunden"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 14, 15, 10), &now),
            "vor 1 Stunde"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 11, 13, 23, 59), &now),
            "gestern"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 10, 12, 9, 0), &now),
            "12. Okt."
        );
        assert_eq!(
            relative_date(Lang::De, &at(2026, 3, 8, 9, 0), &now),
            "8. März"
        );
        assert_eq!(
            relative_date(Lang::De, &at(2025, 10, 2, 9, 0), &now),
            "2. Okt. 2025"
        );
        // Over New Year: yesterday is still "gestern".
        let new_year = at(2027, 1, 1, 0, 10);
        assert_eq!(
            relative_date(Lang::De, &at(2026, 12, 31, 22, 0), &new_year),
            "gestern"
        );
    }

    #[test]
    fn scan_times_read_like_s07() {
        let now = at(2026, 11, 14, 16, 30);
        assert_eq!(
            day_and_time(Lang::De, &at(2026, 11, 14, 9, 12), &now),
            "heute 09:12"
        );
        assert_eq!(
            day_and_time(Lang::De, &at(2026, 11, 13, 21, 5), &now),
            "gestern 21:05"
        );
        assert_eq!(
            day_and_time(Lang::De, &at(2026, 9, 1, 8, 0), &now),
            "1. Sept. 08:00"
        );
    }

    #[test]
    fn the_local_calendar_is_available() {
        let now = local(SystemTime::now()).expect("localtime_r");
        assert!(now.year >= 2024);
        assert!((1..=12).contains(&now.month) && (1..=31).contains(&now.day));
        assert!(local(UNIX_EPOCH - Duration::from_secs(1)).is_none());
    }

    #[test]
    fn plurals_paths_and_latency() {
        let home = Path::new("/home/kim");
        assert_eq!(
            tilde_in(Path::new("/home/kim/Musik/a.komponist"), Some(home)),
            "~/Musik/a.komponist"
        );
        assert_eq!(tilde_in(Path::new("/home/kim"), Some(home)), "~");
        assert_eq!(
            tilde_in(Path::new("/usr/lib/clap"), Some(home)),
            "/usr/lib/clap"
        );
        assert_eq!(
            tilde_in(Path::new("/home/kimberly"), Some(home)),
            "/home/kimberly"
        );
        assert_eq!(
            latency(Lang::De, 256, 48_000, Duration::ZERO),
            "5.3 ms Latenz"
        );
        assert_eq!(
            latency(Lang::De, 256, 48_000, Duration::from_micros(2_000)),
            "7.3 ms Latenz"
        );
    }
}
