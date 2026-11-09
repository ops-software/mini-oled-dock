//! Pure logic of the self-updater: versions, canonical tags, release URLs and
//! the host allow-list. No I/O, so everything here is unit-tested.

// The updater is wired in by later tasks; until then most items are unused.
#![allow(dead_code)]

use std::fmt;

pub const REPO: &str = "ops-software/mini-oled-dock";
pub const LATEST_API_URL: &str =
    "https://api.github.com/repos/ops-software/mini-oled-dock/releases/latest";

/// A `major.minor.patch` version. Field order is semver order, so the derived
/// `Ord` compares versions correctly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `X.Y.Z` or `vX.Y.Z`; each part is 1 to 5 ASCII digits.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let mut next = || {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse::<u32>().ok()
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }

    /// The version this binary was built as (`Default` if it ever fails to parse).
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_default()
    }

    pub fn tag(self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Canonical `vX.Y.Z` only: no leading zeros, no whitespace, no path pieces.
pub fn parse_tag(s: &str) -> Option<Version> {
    let v = Version::parse(s)?;
    (v.tag() == s).then_some(v)
}

pub fn is_newer(tag: &str, current: Version) -> bool {
    parse_tag(tag).is_some_and(|v| v > current)
}

pub fn asset_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{name}")
}

/// Only a canonical tag becomes part of the URL; anything else falls back to
/// the latest-release page.
pub fn release_page_url(tag: Option<&str>) -> String {
    match tag.filter(|t| parse_tag(t).is_some()) {
        Some(t) => format!("https://github.com/{REPO}/releases/tag/{t}"),
        None => format!("https://github.com/{REPO}/releases/latest"),
    }
}

/// `api.github.com`, `github.com` and `*.githubusercontent.com`, any case.
pub fn host_allowed(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "api.github.com" || h == "github.com" {
        return true;
    }
    h.strip_suffix(".githubusercontent.com")
        .is_some_and(|label| !label.is_empty() && !label.starts_with('.'))
}

pub const API_MAX_BYTES: u64 = 1024 * 1024;
pub const EXE_MAX_BYTES: u64 = 20 * 1024 * 1024;
pub const SHA_MAX_BYTES: u64 = 1024;
pub const NOTES_MAX_CHARS: usize = 16 * 1024;
pub const EXE_ASSET: &str = "dock.exe";
pub const SHA_ASSET: &str = "dock.exe.sha256";
pub const FIRST_CHECK_MS: u32 = 60_000;
pub const CHECK_PERIOD_MS: u32 = 86_400_000;
pub const RETRY_MS: u32 = 3_600_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub name: String,
    pub notes: String,
    pub published: String,
    pub exe: Asset,
    pub sha256: Asset,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RawRelease {
    tag_name: String,
    name: String,
    body: String,
    published_at: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<RawAsset>,
}

/// Parse the `releases/latest` JSON. Anything that is not a plain, published
/// release with both assets at their canonical URLs is an error.
pub fn parse_release(json: &[u8]) -> Result<Release, String> {
    if json.len() as u64 > API_MAX_BYTES {
        return Err("release JSON too large".into());
    }
    let raw: RawRelease = serde_json::from_slice(json).map_err(|e| format!("bad JSON: {e}"))?;
    let version = parse_tag(&raw.tag_name).ok_or("non-canonical tag")?;
    if raw.draft || raw.prerelease {
        return Err("draft or pre-release".into());
    }
    let find = |name: &str, max: u64| -> Result<Asset, String> {
        let a = raw
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("asset {name} missing"))?;
        if a.browser_download_url != asset_url(&raw.tag_name, name) {
            return Err(format!("asset {name} has an unexpected URL"));
        }
        if a.size == 0 || a.size > max {
            return Err(format!("asset {name} has an unacceptable size"));
        }
        Ok(Asset {
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let exe = find(EXE_ASSET, EXE_MAX_BYTES)?;
    let sha256 = find(SHA_ASSET, SHA_MAX_BYTES)?;
    let published = if parse_rfc3339_utc(&raw.published_at).is_some() {
        raw.published_at.clone()
    } else {
        String::new()
    };
    Ok(Release {
        tag: raw.tag_name,
        version,
        name: raw.name,
        notes: raw.body.chars().take(NOTES_MAX_CHARS).collect(),
        published,
        exe,
        sha256,
    })
}

/// The `.sha256` asset: `<64 hex>  dock.exe` (either case, optional BOM,
/// optional `*` before the name, any line ending). Only the first line counts.
pub fn parse_sha256_file(bytes: &[u8]) -> Option<[u8; 32]> {
    if bytes.len() as u64 > SHA_MAX_BYTES {
        return None;
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if !bytes.is_ascii() {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.lines().next()?;
    let mut tokens = line.split_whitespace();
    let hex = tokens.next()?;
    let name = tokens.next()?;
    if tokens.next().is_some() || name.strip_prefix('*').unwrap_or(name) != EXE_ASSET {
        return None;
    }
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// `dock.exe --version` prints `mini-oled-dock X.Y.Z`.
pub fn version_output_matches(stdout: &[u8], v: Version) -> bool {
    std::str::from_utf8(stdout).is_ok_and(|s| s.trim() == format!("mini-oled-dock {v}"))
}

/// Staging folders that may be deleted: canonical tags at or below `current`.
pub fn stale_stages(names: &[String], current: Version) -> Vec<String> {
    names
        .iter()
        .filter(|n| parse_tag(n).is_some_and(|v| v <= current))
        .cloned()
        .collect()
}

/// Days since 1970-01-01 to (year, month, day). H. Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// (year, month, day) to days since 1970-01-01. H. Hinnant's days_from_civil.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn rfc3339_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Exactly `YYYY-MM-DDTHH:MM:SSZ` with a valid calendar date.
pub fn parse_rfc3339_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || !s.is_ascii() {
        return None;
    }
    for (i, c) in b.iter().enumerate() {
        let ok = match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        };
        if !ok {
            return None;
        }
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u32>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || d == 0 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let days = days_from_civil(i64::from(y), mo, d);
    // Reject days that roll over into the next month (30 Feb, 29 Feb 2025).
    if civil_from_days(days) != (i64::from(y), mo, d) {
        return None;
    }
    u64::try_from(days * 86_400 + i64::from(h * 3600 + mi * 60 + se)).ok()
}

/// Milliseconds until the next check; 0 means check now. A clock that moved
/// back counts as "never checked". The floor keeps a near-due check from
/// firing right at start-up.
pub fn next_check_delay_ms(now: u64, last: Option<u64>) -> u32 {
    let Some(last) = last.filter(|l| *l <= now) else {
        return 0;
    };
    let elapsed = now - last;
    if elapsed >= 86_400 {
        return 0;
    }
    (((86_400 - elapsed) * 1000) as u32).max(FIRST_CHECK_MS)
}

#[cfg(test)]
pub(crate) const RELEASE_FIXTURE: &str = r#"{"tag_name":"v1.4.1","name":"mini-oled-dock 1.4.1","body":"Fixes","published_at":"2026-11-20T10:00:00Z","draft":false,"prerelease":false,"html_url":"https://evil.example/","assets":[{"name":"dock.exe","size":720896,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe"},{"name":"dock.exe.sha256","size":76,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe.sha256"}]}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_and_order() {
        assert_eq!(
            Version::parse("1.4.0"),
            Some(Version {
                major: 1,
                minor: 4,
                patch: 0
            })
        );
        assert_eq!(Version::parse("v1.4.0"), Version::parse("1.4.0"));
        for bad in [
            "", "1.4", "1.4.0.1", "v1.4.0-rc1", "1.4.0+meta", " 1.4.0", "1.4.x", "1..0", "v-1.4.0",
            "123456.0.0", "V1.4.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version::parse("1.10.0") > Version::parse("1.9.9"));
        assert!(Version::parse("2.0.0") > Version::parse("1.99.99"));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn tags_are_canonical() {
        assert_eq!(parse_tag("v1.4.1"), Version::parse("1.4.1"));
        for bad in [
            "1.4.1",
            "v1.04.1",
            "v01.4.1",
            "../v1.4.1",
            "v1.4.1 ",
            "v1.4.1/",
            r"v1.4.1\x",
        ] {
            assert_eq!(parse_tag(bad), None, "{bad}");
        }
        let cur = Version::parse("1.4.0").unwrap();
        assert!(is_newer("v1.4.1", cur) && is_newer("v1.5.0", cur) && is_newer("v2.0.0", cur));
        assert!(!is_newer("v1.4.0", cur) && !is_newer("v1.3.9", cur) && !is_newer("garbage", cur));
    }

    #[test]
    fn urls_and_hosts() {
        assert_eq!(
            asset_url("v1.4.1", "dock.exe"),
            "https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe"
        );
        assert_eq!(
            release_page_url(Some("v1.4.1")),
            "https://github.com/ops-software/mini-oled-dock/releases/tag/v1.4.1"
        );
        assert_eq!(
            release_page_url(None),
            "https://github.com/ops-software/mini-oled-dock/releases/latest"
        );
        assert_eq!(
            release_page_url(Some("../x")),
            "https://github.com/ops-software/mini-oled-dock/releases/latest"
        );
        for ok in [
            "api.github.com",
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
            "GitHub.com",
        ] {
            assert!(host_allowed(ok), "{ok}");
        }
        for bad in [
            "",
            "evil.com",
            "github.com.evil.com",
            "xgithub.com",
            ".githubusercontent.com",
            "githubusercontent.com.evil",
        ] {
            assert!(!host_allowed(bad), "{bad}");
        }
    }

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"; // SHA-256("abc")

    #[test]
    fn release_json_parses() {
        let r = parse_release(RELEASE_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            (r.tag.as_str(), r.version),
            ("v1.4.1", Version::parse("1.4.1").unwrap())
        );
        assert_eq!(
            (r.notes.as_str(), r.published.as_str()),
            ("Fixes", "2026-11-20T10:00:00Z")
        );
        assert_eq!(
            r.exe,
            Asset {
                url: asset_url("v1.4.1", EXE_ASSET),
                size: 720_896
            }
        );
        assert_eq!(r.sha256.url, asset_url("v1.4.1", SHA_ASSET));
    }

    #[test]
    fn release_json_rejects_hostile_input() {
        let swap = |a: &str, b: &str| RELEASE_FIXTURE.replace(a, b);
        let bad = [
            swap("\"tag_name\":\"v1.4.1\"", "\"tag_name\":\"../../x\""),
            swap("\"prerelease\":false", "\"prerelease\":true"),
            swap("\"draft\":false", "\"draft\":true"),
            swap("download/v1.4.1/dock.exe\"", "download/v1.4.0/dock.exe\""), // URL not for this tag
            swap("\"size\":720896", "\"size\":20971521"),                    // over 20 MB
            swap("\"size\":720896", "\"size\":0"),
            swap("\"name\":\"dock.exe\"", "\"name\":\"other.exe\""), // asset missing
            "<html>captive portal</html>".to_string(),
            "[".repeat(10_000),
            r#"{"tag_name": 5}"#.to_string(),
        ];
        for b in bad {
            assert!(parse_release(b.as_bytes()).is_err(), "{:.60}", b);
        }
        let long = swap("\"Fixes\"", &format!("\"{}\"", "é".repeat(20_000)));
        assert_eq!(
            parse_release(long.as_bytes()).unwrap().notes.chars().count(),
            NOTES_MAX_CHARS
        );
    }

    #[test]
    fn sha256_file_formats() {
        let mut want = [0u8; 32];
        for (i, b) in want.iter_mut().enumerate() {
            *b = u8::from_str_radix(&ABC[2 * i..2 * i + 2], 16).unwrap();
        }
        // release.yml: Out-File -Encoding ascii -> "<hex>  dock.exe\r\n"
        assert_eq!(
            parse_sha256_file(format!("{ABC}  dock.exe\r\n").as_bytes()),
            Some(want)
        );
        assert_eq!(
            parse_sha256_file(format!("{}  dock.exe", ABC.to_uppercase()).as_bytes()),
            Some(want)
        );
        assert_eq!(
            parse_sha256_file(format!("\u{feff}{ABC} *dock.exe\n").as_bytes()),
            Some(want)
        );
        for bad in [
            String::new(),
            ABC.to_string(),
            format!("{ABC}  other.exe"),
            format!("{}  dock.exe", &ABC[1..]),
            format!("{ABC}  dock.exe extra"),
            format!("{ABC}g  dock.exe"),
        ] {
            assert_eq!(parse_sha256_file(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(parse_sha256_file(&[b'a'; 2000]), None);
    }

    #[test]
    fn version_output() {
        let v = Version::parse("1.4.1").unwrap();
        assert!(version_output_matches(b"mini-oled-dock 1.4.1\r\n", v));
        assert!(!version_output_matches(b"mini-oled-dock 1.4.0\r\n", v));
        assert!(!version_output_matches(b"mini-oled-dock 1.4.1 extra", v));
        assert!(!version_output_matches(b"", v));
    }

    #[test]
    fn stale_stage_folders() {
        let names: Vec<String> = ["v1.3.0", "v1.4.0", "v1.4.1", "notes", "v1.04.0"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            stale_stages(&names, Version::parse("1.4.0").unwrap()),
            ["v1.3.0", "v1.4.0"]
        );
    }

    #[test]
    fn rfc3339_and_schedule() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_227_696), "2026-11-09T12:34:56Z");
        assert_eq!(rfc3339_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(
            parse_rfc3339_utc("2026-11-09T12:34:56Z"),
            Some(1_794_227_696)
        );
        for bad in [
            "",
            "2026-11-09",
            "2026-13-01T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-11-09T12:34:56+01:00",
            "2026-11-09T12:34:56.1Z",
        ] {
            assert_eq!(parse_rfc3339_utc(bad), None, "{bad}");
        }
        let now = 1_794_227_696;
        assert_eq!(next_check_delay_ms(now, None), 0);
        assert_eq!(next_check_delay_ms(now, Some(now + 600)), 0); // clock moved back
        assert_eq!(next_check_delay_ms(now, Some(now - 3_600)), 23 * 3_600_000);
        assert_eq!(next_check_delay_ms(now, Some(now - 86_390)), FIRST_CHECK_MS); // floor of 60 s
        assert_eq!(next_check_delay_ms(now, Some(now - 90_000)), 0);
    }
}
