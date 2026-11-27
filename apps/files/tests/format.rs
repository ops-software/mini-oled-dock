//! F1a.1: sizes, counts and dates. The fixed formatters are machine-independent; the system one is checked for shape.

use files_engine::format::{Fmt, LocalTime};

/// A UTC civil time as a FILETIME (100 ns since 1601-01-01).
fn ft(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    ((days + 134_774) * 86_400 + h * 3600 + mi * 60 + s) * 10_000_000
}

const NOW: LocalTime = LocalTime { year: 2026, month: 11, day: 21, hour: 14, minute: 40 };

#[test]
fn size_adaptive_table() {
    let f = Fmt::fixed_en();
    for (n, want) in [
        (0u64, "0 B"),
        (512, "512 B"),
        (1_023, "1,023 B"),
        (1_024, "1.00 KB"),
        (10_076, "9.84 KB"),
        (14_889_779, "14.2 MB"),
        (1_990_116_046_028, "1.81 TB"),
        (1_048_063, "1.00 MB"),
        (1_048_576, "1.00 MB"),
        (10_240, "10.0 KB"),
        (102_400, "100 KB"),
        (1_023_487, "999 KB"),
    ] {
        assert_eq!(f.size(n, false), want, "{n}");
    }
    assert!(!f.size(u64::MAX, false).is_empty());
}

#[test]
fn size_it_uses_the_decimal_comma() {
    assert_eq!(Fmt::fixed_it().size(14_889_779, false), "14,2 MB");
    assert_eq!(Fmt::fixed_it().size(1_023, false), "1.023 B");
}

#[test]
fn size_like_explorer() {
    let f = Fmt::fixed_en();
    assert_eq!(f.size(1, true), "1 KB");
    assert_eq!(f.size(1_263_616, true), "1,234 KB");
    assert_eq!(f.size(0, true), "0 KB");
}

#[test]
fn count_grouping() {
    assert_eq!(Fmt::fixed_en().count(1_204), "1,204");
    assert_eq!(Fmt::fixed_it().count(1_204), "1.204");
    assert_eq!(Fmt::fixed_en().count(0), "0");
    assert_eq!(Fmt::fixed_en().count(999), "999");
    assert_eq!(Fmt::fixed_en().count(1_000_000), "1,000,000");
}

#[test]
fn modified_table() {
    let (en, it) = (Fmt::fixed_en(), Fmt::fixed_it());
    assert_eq!(en.modified(ft(2026, 11, 21, 14, 32, 5), NOW, false), "14:32");
    assert_eq!(en.modified(ft(2026, 11, 20, 18, 2, 0), NOW, false), "Yesterday 18:02");
    assert_eq!(it.modified(ft(2026, 11, 20, 18, 2, 0), NOW, false), "Ieri 18:02");
    assert_eq!(en.modified(ft(2026, 11, 19, 9, 10, 0), NOW, false), "19/11/2026 09:10");
    assert_eq!(it.modified(ft(2026, 11, 20, 9, 10, 0), NOW, true), "20/11/2026 09:10");
    assert_eq!(en.modified(ft(2026, 11, 21, 14, 32, 0), NOW, true), "21/11/2026 14:32");
    assert_eq!(en.modified(-1, NOW, false), "");
    let mut buf = String::from("x ");
    en.modified_into(&mut buf, ft(2026, 11, 21, 8, 5, 0), NOW, false);
    assert_eq!(buf, "x 08:05");
}

#[test]
fn midnight_boundary() {
    let en = Fmt::fixed_en();
    assert_eq!(en.modified(ft(2026, 11, 20, 23, 59, 0), NOW, false), "Yesterday 23:59");
    assert_eq!(en.modified(ft(2026, 11, 21, 0, 1, 0), NOW, false), "00:01");
    let jan = LocalTime { year: 2027, month: 1, day: 1, hour: 0, minute: 5 };
    assert_eq!(en.modified(ft(2026, 12, 31, 23, 50, 0), jan, false), "Yesterday 23:50");
    let mar = LocalTime { year: 2028, month: 3, day: 1, hour: 10, minute: 0 };
    assert_eq!(en.modified(ft(2028, 2, 29, 12, 0, 0), mar, false), "Yesterday 12:00");
}

#[test]
fn full_date_has_seconds() {
    assert_eq!(Fmt::fixed_en().full_date(ft(2026, 11, 20, 9, 10, 7)), "20/11/2026 09:10:07");
}

#[test]
fn fixed_formatters_ignore_the_machine() {
    // The fixed formatters read the file time as UTC and never call NLS: the same strings on every machine.
    let t = ft(2026, 1, 15, 23, 30, 0);
    assert_eq!(Fmt::fixed_en().modified(t, NOW, true), "15/01/2026 23:30");
    assert_eq!(Fmt::fixed_it().modified(t, NOW, true), "15/01/2026 23:30");
    assert_eq!(Fmt::fixed_en().size(14_889_779, false), "14.2 MB");
}

#[test]
fn system_formatter_has_a_sane_shape() {
    let f = Fmt::system(oled_ui::UiLang::EN);
    let s = f.size(10_076, false);
    assert!(s.ends_with(" KB") && s.chars().next().is_some_and(|c| c.is_ascii_digit()), "{s}");
    let c = f.count(1_234_567);
    assert!(c.chars().filter(char::is_ascii_digit).count() == 7, "{c}");
    let now = Fmt::to_local(ft(2026, 11, 21, 12, 0, 0)).expect("local time");
    let m = f.modified(ft(2020, 6, 1, 12, 0, 0), now, false);
    assert!(m.chars().any(|c| c.is_ascii_digit()), "{m}");
    assert!(!f.full_date(ft(2020, 6, 1, 12, 0, 0)).is_empty());
}
