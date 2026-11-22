//! Date and time rows (5.4.10, L2-D1): pure code. The civil date is computed from the Unix time, the day and month
//! names come from `strings`, and the locale's 12/24-hour choice and date order are read once per open (the caller
//! hands them in), so nothing on the key path calls Windows. ")" mode lists every row; Mixed mode answers one of them
//! for an exact keyword.

use super::strings;

/// The largest absolute Unix time the calendar code takes (far beyond year 9999): keeps every product in range.
const MAX_SECS: i64 = 1 << 45;

/// A calendar date and time of day. `wd` is the weekday, 0 = Monday.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Civil {
    pub y: i32,
    pub mo: u8,
    pub d: u8,
    pub h: u8,
    pub mi: u8,
    pub s: u8,
    pub wd: u8,
}

/// The order of the date's parts in the user's short date.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateOrder {
    Dmy,
    Mdy,
    Ymd,
}

/// The locale's choices for the shown date and time, read at each open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeStyle {
    pub h24: bool,
    pub order: DateOrder,
}

impl Default for TimeStyle {
    fn default() -> Self {
        TimeStyle { h24: true, order: DateOrder::Dmy }
    }
}

impl TimeStyle {
    /// From `LOCALE_ITIME` ("0" = 12-hour, "1" = 24-hour) and `LOCALE_SSHORTDATE` ("dd/MM/yyyy"): the order
    /// is that of the first letter of the pattern. Anything else keeps the default.
    pub fn from_locale(itime: &str, short_date: &str) -> TimeStyle {
        let d = TimeStyle::default();
        let order = match short_date.trim().chars().next() {
            Some('d' | 'D') => DateOrder::Dmy,
            Some('M') => DateOrder::Mdy,
            Some('y' | 'Y') => DateOrder::Ymd,
            _ => d.order,
        };
        TimeStyle { h24: itime.trim() != "0", order }
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i32, mo: u8, d: u8) -> i64 {
    let y = i64::from(y) - i64::from(mo <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(mo) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date and time of a Unix time (UTC), proleptic Gregorian.
pub fn civil_from_unix(secs: i64) -> Civil {
    let secs = secs.clamp(-MAX_SECS, MAX_SECS);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    Civil {
        y: i32::try_from(y).unwrap_or(0),
        mo: u8::try_from(mo).unwrap_or(1),
        d: u8::try_from(d).unwrap_or(1),
        h: u8::try_from(rem / 3600).unwrap_or(0),
        mi: u8::try_from(rem % 3600 / 60).unwrap_or(0),
        s: u8::try_from(rem % 60).unwrap_or(0),
        // 1970-01-01 was a Thursday (3 with Monday = 0).
        wd: u8::try_from((days + 3).rem_euclid(7)).unwrap_or(0),
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// The day of the year, 1 = January 1st.
pub fn day_of_year(y: i32, mo: u8, d: u8) -> u16 {
    const BEFORE: [u16; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let before = BEFORE.get(usize::from(mo).saturating_sub(1)).copied().unwrap_or(0);
    let leap = u16::from(is_leap(y) && mo > 2);
    before + leap + u16::from(d)
}

/// 53 for a year that has a week 53, else 52.
fn weeks_in_year(y: i32) -> u8 {
    let p = |y: i32| (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)).rem_euclid(7);
    if p(y) == 4 || p(y - 1) == 3 {
        53
    } else {
        52
    }
}

/// The ISO 8601 week of a date: its week-based year and the week number.
pub fn iso_week(y: i32, mo: u8, d: u8) -> (i32, u8) {
    let wd = (days_from_civil(y, mo, d) + 3).rem_euclid(7) + 1;
    let week = (i64::from(day_of_year(y, mo, d)) - wd + 10) / 7;
    if week < 1 {
        (y - 1, weeks_in_year(y - 1))
    } else if week > i64::from(weeks_in_year(y)) {
        (y + 1, 1)
    } else {
        (y, u8::try_from(week).unwrap_or(1))
    }
}

/// How many rows ")" mode lists.
pub const ROWS: usize = 8;

/// One date and time row: a stable id (the item key), the value shown (and copied) and what it is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub id: &'static str,
    pub value: String,
    pub label: String,
}

const IDS: [&str; ROWS] = ["long", "short", "seconds", "isoweek", "dayofyear", "utc", "unix", "iso8601"];

/// The time of day, "17:05" or "5:05 PM"; seconds when asked.
fn clock_text(c: &Civil, h24: bool, seconds: bool) -> String {
    let (h, suffix) = if h24 {
        (format!("{:02}", c.h), "")
    } else {
        let h12 = match c.h % 12 {
            0 => 12,
            h => h,
        };
        (h12.to_string(), if c.h < 12 { " AM" } else { " PM" })
    };
    if seconds {
        format!("{h}:{:02}:{:02}{suffix}", c.mi, c.s)
    } else {
        format!("{h}:{:02}{suffix}", c.mi)
    }
}

fn short_date(c: &Civil, order: DateOrder) -> String {
    match order {
        DateOrder::Dmy => format!("{:02}/{:02}/{}", c.d, c.mo, c.y),
        DateOrder::Mdy => format!("{:02}/{:02}/{}", c.mo, c.d, c.y),
        DateOrder::Ymd => format!("{}-{:02}-{:02}", c.y, c.mo, c.d),
    }
}

/// "+01:00" for 60.
fn offset_text(offset_min: i32) -> String {
    let sign = if offset_min < 0 { '-' } else { '+' };
    let a = offset_min.unsigned_abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

/// Row `i` (0..8) of `rows`; None past the last.
pub fn row(i: usize, now_unix: i64, offset_min: i32, style: TimeStyle) -> Option<Row> {
    let id = *IDS.get(i)?;
    let local = now_unix.clamp(-MAX_SECS, MAX_SECS).saturating_add(i64::from(offset_min) * 60);
    let c = civil_from_unix(local);
    let (value, label) = match i {
        0 => {
            let (wd, mo) = (strings::day_name(c.wd), strings::month_name(c.mo));
            let time = clock_text(&c, style.h24, false);
            let date = match style.order {
                DateOrder::Mdy => format!("{wd}, {mo} {}, {}", c.d, c.y),
                _ => format!("{wd} {} {mo} {}", c.d, c.y),
            };
            (format!("{date}, {time}"), strings::label_long_date().to_string())
        }
        1 => (short_date(&c, style.order), strings::label_short_date().to_string()),
        2 => (clock_text(&c, style.h24, true), strings::label_time_seconds().to_string()),
        3 => {
            let (wy, w) = iso_week(c.y, c.mo, c.d);
            (strings::week_number(w), format!("{} · {wy}-W{w:02}", strings::label_iso_week()))
        }
        4 => {
            let n = day_of_year(c.y, c.mo, c.d);
            (strings::day_of_year(n), strings::label_day_of_year().to_string())
        }
        5 => {
            let u = civil_from_unix(now_unix);
            (
                format!("{}-{:02}-{:02} {:02}:{:02}:{:02} {}", u.y, u.mo, u.d, u.h, u.mi, u.s, strings::label_utc()),
                strings::label_utc().to_string(),
            )
        }
        6 => (now_unix.clamp(-MAX_SECS, MAX_SECS).to_string(), strings::label_unix().to_string()),
        _ => (
            format!("{}-{:02}-{:02}T{:02}:{:02}:{:02}{}", c.y, c.mo, c.d, c.h, c.mi, c.s, offset_text(offset_min)),
            strings::label_iso8601().to_string(),
        ),
    };
    Some(Row { id, value, label })
}

/// The eight rows, in the order of ")" mode: long date and time, short date, time with seconds, ISO week, day of the
/// year, UTC time, Unix time, ISO 8601 with the offset.
#[cfg(test)]
pub fn rows(now_unix: i64, offset_min: i32, style: TimeStyle) -> Vec<Row> {
    (0..ROWS).filter_map(|i| row(i, now_unix, offset_min, style)).collect()
}

/// The row an exact keyword of Mixed mode answers with: the long date and time, or the ISO week. None for anything
/// else, without allocating.
pub fn keyword_row(folded: &str) -> Option<usize> {
    match folded {
        "time" | "date" | "today" | "now" | "ora" | "data" | "oggi" | "adesso" => Some(0),
        "week" | "settimana" => Some(3),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::i18n::with_italian;

    /// 2026-11-22 16:05:09 UTC, a Sunday.
    const NOW: i64 = 1_795_363_509;
    const DMY24: TimeStyle = TimeStyle { h24: true, order: DateOrder::Dmy };

    fn value(i: usize, off: i32, style: TimeStyle) -> String {
        row(i, NOW, off, style).unwrap().value
    }

    #[test]
    fn iso_week_vectors() {
        assert_eq!(iso_week(2026, 1, 1), (2026, 1));
        assert_eq!(iso_week(2027, 1, 1), (2026, 53));
        assert_eq!(iso_week(2021, 1, 3), (2020, 53));
        assert_eq!(iso_week(2024, 12, 30), (2025, 1));
        assert_eq!(iso_week(2026, 11, 22), (2026, 47));
    }

    #[test]
    fn day_of_year_vectors() {
        assert_eq!(day_of_year(2024, 12, 31), 366);
        assert_eq!(day_of_year(2026, 3, 1), 60);
        assert_eq!(day_of_year(2026, 12, 31), 365);
        assert_eq!(day_of_year(2024, 2, 29), 60);
        assert_eq!(day_of_year(2026, 1, 1), 1);
    }

    #[test]
    fn civil_from_unix_edges() {
        let c = |s| civil_from_unix(s);
        assert_eq!(c(0), Civil { y: 1970, mo: 1, d: 1, h: 0, mi: 0, s: 0, wd: 3 });
        assert_eq!(c(-1), Civil { y: 1969, mo: 12, d: 31, h: 23, mi: 59, s: 59, wd: 2 });
        assert_eq!(c(1 << 31), Civil { y: 2038, mo: 1, d: 19, h: 3, mi: 14, s: 8, wd: 1 });
        assert_eq!(c(253_402_300_799), Civil { y: 9999, mo: 12, d: 31, h: 23, mi: 59, s: 59, wd: 4 });
        assert_eq!(c(NOW), Civil { y: 2026, mo: 11, d: 22, h: 16, mi: 5, s: 9, wd: 6 });
        // A leap day and the first second after it.
        assert_eq!(c(1_709_164_800), Civil { y: 2024, mo: 2, d: 29, h: 0, mi: 0, s: 0, wd: 3 });
        // Far out of range never panics.
        let _ = (c(i64::MIN), c(i64::MAX));
    }

    #[test]
    fn long_date_it_en() {
        assert_eq!(with_italian(true, || value(0, 60, DMY24)), "domenica 22 novembre 2026, 17:05");
        assert_eq!(with_italian(false, || value(0, 60, DMY24)), "Sunday 22 November 2026, 17:05");
        let us = TimeStyle { h24: false, order: DateOrder::Mdy };
        assert_eq!(with_italian(false, || value(0, 60, us)), "Sunday, November 22, 2026, 5:05 PM");
    }

    #[test]
    fn twelve_hour_and_orders() {
        let h12 = |order| TimeStyle { h24: false, order };
        assert_eq!(value(2, 60, h12(DateOrder::Dmy)), "5:05:09 PM");
        assert_eq!(value(2, 60, DMY24), "17:05:09");
        // Midnight and noon, and the zero padding of the 24-hour clock.
        let at = |secs: i64, style| row(2, secs, 0, style).unwrap().value;
        assert_eq!(at(0, h12(DateOrder::Dmy)), "12:00:00 AM");
        assert_eq!(at(12 * 3600, h12(DateOrder::Dmy)), "12:00:00 PM");
        assert_eq!(at(9 * 3600 + 7 * 60, DMY24), "09:07:00");
        assert_eq!(value(1, 60, DMY24), "22/11/2026");
        assert_eq!(value(1, 60, TimeStyle { h24: true, order: DateOrder::Mdy }), "11/22/2026");
        assert_eq!(value(1, 60, TimeStyle { h24: true, order: DateOrder::Ymd }), "2026-11-22");
    }

    #[test]
    fn offset_applies_before_civil() {
        // 23:30 UTC on the 22nd is already the 23rd at +01:00 and still the 22nd at -05:00.
        let late = 1_795_390_200;
        assert_eq!(row(1, late, 60, DMY24).unwrap().value, "23/11/2026");
        assert_eq!(row(1, late, -300, DMY24).unwrap().value, "22/11/2026");
        assert_eq!(row(7, late, 60, DMY24).unwrap().value, "2026-11-23T00:30:00+01:00");
        assert_eq!(row(7, late, -330, DMY24).unwrap().value, "2026-11-22T18:00:00-05:30");
        assert_eq!(row(7, late, 0, DMY24).unwrap().value, "2026-11-22T23:30:00+00:00");
        // UTC and Unix time ignore the offset.
        assert_eq!(value(5, 60, DMY24), "2026-11-22 16:05:09 UTC");
        assert_eq!(value(6, 60, DMY24), NOW.to_string());
    }

    #[test]
    fn the_eight_rows_and_their_labels() {
        let r = rows(NOW, 60, DMY24);
        let ids: Vec<&str> = r.iter().map(|r| r.id).collect();
        assert_eq!(ids, ["long", "short", "seconds", "isoweek", "dayofyear", "utc", "unix", "iso8601"]);
        assert_eq!(r[3].value, "Week 47");
        assert_eq!(r[3].label, "ISO week · 2026-W47");
        assert_eq!(r[4].value, "Day 326 of the year");
        assert_eq!(with_italian(true, || value(3, 60, DMY24)), "Settimana 47");
        assert_eq!(with_italian(true, || value(4, 60, DMY24)), "Giorno 326 dell'anno");
        assert!(row(8, NOW, 0, DMY24).is_none());
        assert!(r.iter().all(|r| !r.value.is_empty() && !r.label.is_empty()));
    }

    #[test]
    fn keywords_and_locale_styles() {
        for k in ["time", "date", "today", "now", "ora", "data", "oggi", "adesso"] {
            assert_eq!(keyword_row(k), Some(0), "{k}");
        }
        assert_eq!(keyword_row("week"), Some(3));
        assert_eq!(keyword_row("settimana"), Some(3));
        for k in ["", "tim", "times", "Time", "time ", "calc"] {
            assert_eq!(keyword_row(k), None, "{k:?}");
        }
        assert_eq!(TimeStyle::from_locale("1", "dd/MM/yyyy"), TimeStyle { h24: true, order: DateOrder::Dmy });
        assert_eq!(TimeStyle::from_locale("0", "M/d/yyyy"), TimeStyle { h24: false, order: DateOrder::Mdy });
        assert_eq!(TimeStyle::from_locale("1", "yyyy-MM-dd"), TimeStyle { h24: true, order: DateOrder::Ymd });
        assert_eq!(TimeStyle::from_locale("", ""), TimeStyle::default());
    }
}
