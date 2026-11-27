//! Posts the registered Updated message to every Tasks window that runs a given exe (the live suite session's tool):
//! post_updated <path to tasks.exe> [major.minor.patch]. The version defaults to this build's; the message proves
//! nothing, Tasks looks at the file on disk itself. Prints how many windows were posted to. Exit 0 when at least one
//! was, 1 when none, 2 for bad arguments.

use std::path::{Path, PathBuf};

use oled_trust::manifest::release::Version;

/// The exe path and the version, from the arguments after the program name.
fn parse(args: &[String]) -> Result<(PathBuf, Version), String> {
    match args {
        [exe] => Ok((PathBuf::from(exe), Version::current())),
        [exe, v] => Ok((PathBuf::from(exe), Version::parse(v).ok_or_else(|| format!("not a version: {v}"))?)),
        _ => Err("usage: post_updated <path to tasks.exe> [major.minor.patch]".to_string()),
    }
}

fn post(exe: &Path, v: Version) -> u32 {
    oled_sys::selftest::notify_updated(oled_trust::frame::tasks::WINDOW_CLASS, false, exe, (v.major, v.minor, v.patch))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (exe, v) = match parse(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    };
    let n = post(&exe, v);
    println!("posted to {n} window(s)");
    std::process::exit(i32::from(n == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_parser_reads_a_path_and_an_optional_version() {
        let (p, v) = parse(&args(&["C:\\x\\tasks.exe"])).unwrap();
        assert_eq!(p, PathBuf::from("C:\\x\\tasks.exe"));
        assert_eq!(v, Version::current());
        let (_, v) = parse(&args(&["a.exe", "1.8.0"])).unwrap();
        assert_eq!(v, Version { major: 1, minor: 8, patch: 0 });
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["a.exe", "one"])).is_err());
        assert!(parse(&args(&["a.exe", "1.2.3", "extra"])).is_err());
    }
}
