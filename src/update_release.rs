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
}
