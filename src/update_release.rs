//! The release rules live in oled-trust; the dock keeps this path and the fixtures only it reads.

pub use oled_trust::manifest::release::*;

#[cfg(test)]
pub(crate) const RELEASE_FIXTURE: &str = r#"{"tag_name":"v1.4.1","name":"mini-oled-dock 1.4.1","body":"Fixes","published_at":"2026-11-20T10:00:00Z","draft":false,"prerelease":false,"html_url":"https://evil.example/","assets":[{"name":"dock.exe","size":720896,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe"},{"name":"dock.exe.sha256","size":76,"browser_download_url":"https://github.com/ops-software/mini-oled-dock/releases/download/v1.4.1/dock.exe.sha256"}]}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_package_version() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    /// GitHub's real answer for v1.5.0 (`author` and `uploader` removed) and that release's
    /// `dock.exe.sha256` as published: the digest is read from the live format and agrees with the
    /// `.sha256` asset.
    #[test]
    fn the_published_release_parses_with_its_digest() {
        const JSON: &[u8] = include_bytes!("../tests/fixtures/update/v1.5.0-release.json");
        const SHA: &[u8] = include_bytes!("../tests/fixtures/update/v1.5.0-dock.exe.sha256");
        let rel = parse_release(JSON).unwrap();
        assert_eq!((rel.tag.as_str(), rel.version), ("v1.5.0", Version::parse("1.5.0").unwrap()));
        assert_eq!(rel.sha256.size, SHA.len() as u64);
        let raw: serde_json::Value = serde_json::from_slice(JSON).unwrap();
        let assets = raw["assets"].as_array().unwrap();
        let sent = &assets.iter().find(|a| a["name"] == EXE_ASSET).unwrap()["digest"];
        match sent {
            serde_json::Value::String(_) => assert_eq!(rel.exe.digest, Some(parse_sha256_file(SHA).unwrap())),
            serde_json::Value::Null => assert_eq!(rel.exe.digest, None),
            other => panic!("unexpected digest {other}"),
        }
    }
}
