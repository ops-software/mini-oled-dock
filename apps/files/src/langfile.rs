//! The installed it.lang (the one start-up call P2-plumbing names): read once, only by an Italian UI of an installed
//! image, and only when it is tagged "it" with the running suite version. Any failure leaves the compiled table and
//! writes one log line.

use std::path::Path;

use oled_sys::lang::{LangFile, LANG_MAX_BYTES};
use oled_trust::manifest::release::Version;

/// True when the folder is an installed image: dock.exe and suite.json sit beside files.exe.
fn is_installed(dir: &Path) -> bool {
    dir.join("dock.exe").is_file() && dir.join("suite.json").is_file()
}

/// The checked language file, or why it is not used. Pure over its inputs: only an Italian UI of an installed image
/// reads the file, and only a file tagged "it" with the running suite version is accepted.
fn lang_file_rules(
    italian: bool,
    installed: bool,
    bytes: impl FnOnce() -> std::io::Result<Vec<u8>>,
    version: Version,
) -> Result<Option<LangFile>, String> {
    if !italian || !installed {
        return Ok(None);
    }
    let f = match bytes() {
        Ok(b) => LangFile::parse(b).map_err(|e| format!("{e:?}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if f.tag() != "it" {
        return Err("tag".to_string());
    }
    if f.suite_version() != (version.major as u16, version.minor as u16, version.patch as u16) {
        return Err("version".to_string());
    }
    Ok(Some(f))
}

/// <install dir>\lang\it.lang for the installed image with the suite version; any failure leaves the compiled table
/// and writes one log line.
pub fn load_lang_file(install_dir: Option<&Path>, italian: bool) {
    let Some(dir) = install_dir else { return };
    let read = || oled_sys::datadir::read_file(&dir.join("lang").join("it.lang"), LANG_MAX_BYTES as u64, true);
    match lang_file_rules(italian, is_installed(dir), read, Version::current()) {
        Ok(Some(f)) => {
            oled_sys::i18n::set_lang_file(Box::leak(Box::new(f)));
        }
        Ok(None) => {}
        Err(why) => oled_sys::log!("files: lang\\it.lang not used ({why})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    #[test]
    fn lang_file_rules_checks_every_condition() {
        let cur = v(1, 8, 0);
        let enc = |tag: &str, ver: (u16, u16, u16)| oled_sys::lang::encode(tag, ver, &[(1, "a")]).unwrap();
        let ok = || Ok(enc("it", (1, 8, 0)));
        let boom = || -> std::io::Result<Vec<u8>> { panic!("must not read") };
        assert!(lang_file_rules(true, true, ok, cur).unwrap().is_some());
        assert!(lang_file_rules(false, true, boom, cur).unwrap().is_none(), "not Italian: not read");
        assert!(lang_file_rules(true, false, boom, cur).unwrap().is_none(), "not installed: not read");
        assert_eq!(lang_file_rules(true, true, || Ok(enc("en", (1, 8, 0))), cur).err().as_deref(), Some("tag"));
        let newer = || Ok(enc("it", (1, 8, 1)));
        assert_eq!(lang_file_rules(true, true, newer, cur).err().as_deref(), Some("version"));
        assert!(lang_file_rules(true, true, || Ok(vec![0; 8]), cur).is_err(), "garbage does not parse");
        let missing = || Err(std::io::ErrorKind::NotFound.into());
        assert!(lang_file_rules(true, true, missing, cur).unwrap().is_none(), "no file: silent");
    }

    #[test]
    fn a_folder_without_the_suite_is_not_installed() {
        assert!(!is_installed(Path::new("Z:\\no\\such\\folder")));
    }
}
