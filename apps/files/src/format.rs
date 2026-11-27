//! Numbers, sizes and dates as the user's locale writes them (P3-files-app 2.2, F1a.1). The fixed English and Italian
//! formatters are machine-independent, for goldens and tests: they never call NLS and read a FILETIME as UTC. The
//! system formatter reads the user's separators once and formats dates through GetDateFormatEx and GetTimeFormatEx
//! after the local conversion. The `_into` variants append to a caller's buffer and allocate nothing themselves.

use core::fmt::Write as _;

use oled_ui::UiLang;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_LONGDATE, DATE_SHORTDATE, ENUM_DATE_FORMATS_FLAGS,
    LOCALE_SDECIMAL, LOCALE_SGROUPING, LOCALE_STHOUSAND, TIME_FORMAT_FLAGS, TIME_NOSECONDS,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use crate::tr::{self, k};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// Decimal and thousands separators, the grouping, the short-date and time pictures and the UI language.
#[derive(Clone, Debug, PartialEq)]
pub struct Fmt {
    decimal: char,
    thousands: char,
    /// Digits in the first (lowest) group; 0 = no grouping.
    grouping: u8,
    date_picture: String,
    time_picture: String,
    lang: UiLang,
    /// Fixed formatters read a FILETIME as UTC and use the pictures; the system one converts to local time and asks NLS.
    system: bool,
}

/// Seconds between 1601-01-01 and 1970-01-01.
const EPOCH_1601_TO_1970_DAYS: i64 = 134_774;

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A FILETIME read as UTC: date and time with seconds.
fn utc_parts(ft: i64) -> Option<(LocalTime, u8)> {
    if ft < 0 {
        return None;
    }
    let secs = ft / 10_000_000;
    let days = secs / 86_400 - EPOCH_1601_TO_1970_DAYS;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let y = u16::try_from(y).ok()?;
    let t =
        LocalTime { year: y, month: m as u8, day: d as u8, hour: (rem / 3600) as u8, minute: (rem % 3600 / 60) as u8 };
    Some((t, (rem % 60) as u8))
}

fn day_number(t: LocalTime) -> i64 {
    days_from_civil(i64::from(t.year), i64::from(t.month), i64::from(t.day))
}

fn system_time_of(t: LocalTime, second: u8) -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: t.year,
        wMonth: u16::from(t.month),
        wDay: u16::from(t.day),
        wHour: u16::from(t.hour),
        wMinute: u16::from(t.minute),
        wSecond: u16::from(second),
        ..SYSTEMTIME::default()
    }
}

/// One NLS string of the user's locale, empty when the call fails.
fn locale_string(lctype: u32) -> String {
    let mut buf = [0u16; 32];
    // SAFETY: `buf` is writable and its length is passed; a null name is LOCALE_NAME_USER_DEFAULT.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), lctype, Some(&mut buf)) };
    if n > 1 {
        String::from_utf16_lossy(&buf[..(n as usize - 1).min(buf.len())])
    } else {
        String::new()
    }
}

/// The first group size of an LOCALE_SGROUPING string ("3;0" and "3;2;0" give 3; "0" gives 0 = none).
fn first_group(s: &str) -> Option<u8> {
    s.split(';').next()?.trim().parse::<u8>().ok()
}

/// Appends `digits` (ASCII) with `sep` between groups of `group` from the right.
fn push_grouped(out: &mut String, digits: &str, sep: char, group: u8) {
    let g = usize::from(group);
    if g == 0 {
        out.push_str(digits);
        return;
    }
    let n = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(g) {
            out.push(sep);
        }
        out.push(c);
    }
}

/// A fixed picture (dd, MM, yyyy, HH, mm, ss) filled from a time.
fn push_picture(out: &mut String, picture: &str, t: LocalTime, second: u8) {
    let mut rest = picture;
    while !rest.is_empty() {
        let (token, n): (&str, usize) = ["yyyy", "dd", "MM", "HH", "mm", "ss"]
            .iter()
            .find(|p| rest.starts_with(**p))
            .map_or(("", rest.chars().next().map_or(1, char::len_utf8)), |p| (*p, p.len()));
        let _ = match token {
            "yyyy" => write!(out, "{:04}", t.year),
            "dd" => write!(out, "{:02}", t.day),
            "MM" => write!(out, "{:02}", t.month),
            "HH" => write!(out, "{:02}", t.hour),
            "mm" => write!(out, "{:02}", t.minute),
            "ss" => write!(out, "{second:02}"),
            _ => {
                out.push_str(&rest[..n]);
                Ok(())
            }
        };
        rest = &rest[n..];
    }
}

impl Fmt {
    /// LOCALE_NAME_USER_DEFAULT through GetLocaleInfoEx, pictures read once; a value the system does not give keeps
    /// the fixed formatter's.
    pub fn system(lang: UiLang) -> Fmt {
        let mut f = if lang.italian { Fmt::fixed_it() } else { Fmt::fixed_en() };
        f.lang = lang;
        f.system = true;
        if let Some(c) = locale_string(LOCALE_SDECIMAL).chars().next() {
            f.decimal = c;
        }
        if let Some(c) = locale_string(LOCALE_STHOUSAND).chars().next() {
            f.thousands = c;
        }
        if let Some(g) = first_group(&locale_string(LOCALE_SGROUPING)) {
            f.grouping = g;
        }
        f
    }

    /// "1,204", "20/11/2026 09:10".
    pub fn fixed_en() -> Fmt {
        Fmt {
            decimal: '.',
            thousands: ',',
            grouping: 3,
            date_picture: "dd/MM/yyyy".into(),
            time_picture: "HH:mm".into(),
            lang: UiLang::EN,
            system: false,
        }
    }

    /// "1.204", "20/11/2026 09:10".
    pub fn fixed_it() -> Fmt {
        Fmt {
            decimal: ',',
            thousands: '.',
            grouping: 3,
            date_picture: "dd/MM/yyyy".into(),
            time_picture: "HH:mm".into(),
            lang: UiLang::IT,
            system: false,
        }
    }

    /// Grouping by locale: "1,204" / "1.204".
    pub fn count(&self, n: u64) -> String {
        let mut s = String::new();
        self.count_into(&mut s, n);
        s
    }

    /// count() appended to `out`.
    pub fn count_into(&self, out: &mut String, n: u64) {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        let mut v = n;
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        // The digits are ASCII.
        push_grouped(out, core::str::from_utf8(&digits[i..]).unwrap_or("0"), self.thousands, self.grouping);
    }

    /// "512 B", "9.84 KB", "14.2 MB" (1,024 base, 3 significant digits); like_explorer: "1,234 KB" rounded up.
    pub fn size(&self, bytes: u64, like_explorer: bool) -> String {
        let mut s = String::new();
        self.size_into(&mut s, bytes, like_explorer);
        s
    }

    /// size() appended to `out`.
    pub fn size_into(&self, out: &mut String, bytes: u64, like_explorer: bool) {
        if like_explorer {
            self.count_into(out, bytes.div_ceil(1024));
            out.push_str(" KB");
            return;
        }
        if bytes < 1024 {
            self.count_into(out, bytes);
            out.push_str(" B");
            return;
        }
        const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
        let mut v = bytes as f64 / 1024.0;
        let mut unit = 0;
        while v >= 999.5 && unit + 1 < UNITS.len() {
            v /= 1024.0;
            unit += 1;
        }
        // Round half up at the digits the magnitude shows; a value that rounds up to the next width keeps that width.
        let (scaled, decimals) = if v < 9.995 {
            ((v * 100.0 + 0.5).floor() as u64, 2)
        } else if v < 99.95 {
            ((v * 10.0 + 0.5).floor() as u64, 1)
        } else {
            ((v + 0.5).floor() as u64, 0)
        };
        let pow = 10u64.pow(decimals);
        let mut int = String::new();
        self.count_into(&mut int, scaled / pow);
        out.push_str(&int);
        if decimals > 0 {
            out.push(self.decimal);
            let _ = write!(out, "{:0width$}", scaled % pow, width = decimals as usize);
        }
        out.push(' ');
        out.push_str(UNITS[unit]);
    }

    /// "14:32" today, "Yesterday 18:02", else short date and time; `full` always writes date and time.
    pub fn modified(&self, ft: i64, now: LocalTime, full: bool) -> String {
        let mut s = String::new();
        self.modified_into(&mut s, ft, now, full);
        s
    }

    /// modified() appended to `out`; nothing is appended for a time that cannot be read.
    pub fn modified_into(&self, out: &mut String, ft: i64, now: LocalTime, full: bool) {
        let Some((t, second)) = self.parts(ft) else { return };
        if !full {
            let (dt, dn) = (day_number(t), day_number(now));
            if dt == dn {
                self.push_time(out, t, second, false);
                return;
            }
            if dt + 1 == dn {
                self.push_yesterday(out, t, second);
                return;
            }
        }
        self.push_date(out, t, second, false);
        out.push(' ');
        self.push_time(out, t, second, false);
    }

    /// The tooltip: long date and time with seconds.
    pub fn full_date(&self, ft: i64) -> String {
        let Some((t, second)) = self.parts(ft) else { return String::new() };
        let mut s = String::new();
        self.push_date(&mut s, t, second, true);
        s.push(' ');
        self.push_time(&mut s, t, second, true);
        s
    }

    /// FileTimeToSystemTime and SystemTimeToTzSpecificLocalTime: the DST of that date.
    pub fn to_local(ft: i64) -> Option<LocalTime> {
        local_parts(ft).map(|(t, _)| t)
    }

    /// The date, time and second a file time shows in this formatter's zone.
    fn parts(&self, ft: i64) -> Option<(LocalTime, u8)> {
        if self.system {
            local_parts(ft)
        } else {
            utc_parts(ft)
        }
    }

    fn push_yesterday(&self, out: &mut String, t: LocalTime, second: u8) {
        if self.lang.pseudo {
            let mut time = String::new();
            self.push_time(&mut time, t, second, false);
            out.push_str(&tr::tf(k::FILES_DATE_YESTERDAY, self.lang, &[("time", &time)]));
            return;
        }
        let template = tr::gen::TABLE.get(k::FILES_DATE_YESTERDAY, self.lang.italian);
        match template.split_once("{time}") {
            Some((a, b)) => {
                out.push_str(a);
                self.push_time(out, t, second, false);
                out.push_str(b);
            }
            None => out.push_str(template),
        }
    }

    fn push_date(&self, out: &mut String, t: LocalTime, second: u8, long: bool) {
        if !self.system {
            push_picture(out, &self.date_picture, t, second);
            return;
        }
        let st = system_time_of(t, second);
        let flags = ENUM_DATE_FORMATS_FLAGS(if long { DATE_LONGDATE.0 } else { DATE_SHORTDATE.0 });
        let mut buf = [0u16; 96];
        // SAFETY: `st` and `buf` are valid for the call; null name = the user's locale, null picture = by flags.
        let n = unsafe {
            GetDateFormatEx(PCWSTR::null(), flags, Some(&st), PCWSTR::null(), Some(&mut buf), PCWSTR::null())
        };
        push_utf16(out, &buf, n);
    }

    fn push_time(&self, out: &mut String, t: LocalTime, second: u8, seconds: bool) {
        if !self.system {
            push_picture(out, &self.time_picture, t, second);
            if seconds {
                out.push(':');
                let _ = write!(out, "{second:02}");
            }
            return;
        }
        let st = system_time_of(t, second);
        let flags = if seconds { TIME_FORMAT_FLAGS(0) } else { TIME_NOSECONDS };
        let mut buf = [0u16; 48];
        // SAFETY: `st` and `buf` are valid for the call; null name = the user's locale, null picture = by flags.
        let n = unsafe { GetTimeFormatEx(PCWSTR::null(), flags, Some(&st), PCWSTR::null(), Some(&mut buf)) };
        push_utf16(out, &buf, n);
    }
}

/// Appends the `n`-character (NUL included) result of an NLS call; nothing when it failed.
fn push_utf16(out: &mut String, buf: &[u16], n: i32) {
    if n > 1 {
        let len = (n as usize - 1).min(buf.len());
        out.extend(char::decode_utf16(buf[..len].iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')));
    }
}

/// FILETIME to the local zone's date, time and second.
fn local_parts(ft: i64) -> Option<(LocalTime, u8)> {
    if ft < 0 {
        return None;
    }
    let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: ((ft as u64) >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    // SAFETY: `f` and `utc` are valid for the call.
    unsafe { FileTimeToSystemTime(&f, &mut utc) }.ok()?;
    let mut local = SYSTEMTIME::default();
    // SAFETY: a null zone is the current one; `utc` and `local` are valid.
    unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.ok()?;
    let t = LocalTime {
        year: local.wYear,
        month: local.wMonth as u8,
        day: local.wDay as u8,
        hour: local.wHour as u8,
        minute: local.wMinute as u8,
    };
    Some((t, local.wSecond as u8))
}
