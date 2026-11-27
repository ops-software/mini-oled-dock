//! The command line of tasks.exe (F-T2): the show arguments of F-T1, or one of the modes handled before anything
//! else (--selftest --signal <nonce>, --preview <dir> [--scene <name>] [--lang en|it|pseudo] [--dpi <n>], --list-apps,
//! --version). --preview and --list-apps parse in every build; a build without the preview feature refuses them in
//! main (exit 2).

use std::path::PathBuf;

use oled_trust::frame::tasks::{parse_show, ArgError, Show};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    Run(Show),
    Selftest { nonce: String },
    Preview { dir: PathBuf, scene: Option<String>, lang: PreviewLang, dpi: Option<u32> },
    ListApps,
    Version,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewLang {
    #[default]
    En,
    It,
    Pseudo,
}

/// The longest nonce --signal takes (the waiter's nonces are 32 hex digits).
const NONCE_MAX: usize = 64;
/// The DPI range --preview renders at.
const DPI_MIN: u32 = 48;
const DPI_MAX: u32 = 960;

/// The arguments after the program name. Err holds one English line for the console (exit 2).
pub fn parse(args: &[String]) -> Result<Cli, String> {
    match args.first().map(String::as_str) {
        Some("--selftest") => selftest(args.get(1..).unwrap_or_default()),
        Some("--preview") => preview(args.get(1..).unwrap_or_default()),
        Some("--list-apps") => only(args, Cli::ListApps),
        Some("--version") => only(args, Cli::Version),
        _ => parse_show(args).map(Cli::Run).map_err(|e| arg_error(&e)),
    }
}

/// A mode flag that takes nothing after it.
fn only(args: &[String], cli: Cli) -> Result<Cli, String> {
    match args.get(1) {
        None => Ok(cli),
        Some(extra) => Err(format!("unexpected argument {extra:?}")),
    }
}

/// --selftest --signal <nonce>: the nonce is 1 to 64 ASCII letters or digits.
fn selftest(rest: &[String]) -> Result<Cli, String> {
    match rest {
        [flag, nonce] if flag == "--signal" => {
            if nonce.is_empty() || nonce.len() > NONCE_MAX || !nonce.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err("--signal wants a nonce of letters and digits".to_string());
            }
            Ok(Cli::Selftest { nonce: nonce.clone() })
        }
        [flag] if flag == "--signal" => Err("--signal wants a value".to_string()),
        [] => Err("--selftest wants --signal <nonce>".to_string()),
        [other, ..] => Err(format!("unexpected argument {other:?}")),
    }
}

/// --preview <dir> [--scene <name>] [--lang en|it|pseudo] [--dpi <n>], each option at most once, in any order.
fn preview(rest: &[String]) -> Result<Cli, String> {
    let Some(dir) = rest.first().filter(|d| !d.is_empty() && !d.starts_with("--")) else {
        return Err("--preview wants a folder".to_string());
    };
    let (mut scene, mut lang, mut dpi) = (None, None, None);
    let mut it = rest.iter().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} wants a value"))?;
        match flag.as_str() {
            "--scene" if scene.is_none() => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(format!("bad scene name {value:?}"));
                }
                scene = Some(value.clone());
            }
            "--lang" if lang.is_none() => {
                lang = Some(match value.as_str() {
                    "en" => PreviewLang::En,
                    "it" => PreviewLang::It,
                    "pseudo" => PreviewLang::Pseudo,
                    _ => return Err(format!("bad language {value:?} (en, it or pseudo)")),
                });
            }
            "--dpi" if dpi.is_none() => {
                let n = value.parse::<u32>().ok().filter(|n| (DPI_MIN..=DPI_MAX).contains(n));
                dpi = Some(n.ok_or_else(|| format!("bad DPI {value:?} ({DPI_MIN} to {DPI_MAX})"))?);
            }
            "--scene" | "--lang" | "--dpi" => return Err(format!("{flag} given twice")),
            _ => return Err(format!("unexpected argument {flag:?}")),
        }
    }
    Ok(Cli::Preview { dir: PathBuf::from(dir), scene, lang: lang.unwrap_or_default(), dpi })
}

fn arg_error(e: &ArgError) -> String {
    match e {
        ArgError::Unknown(a) => format!("unknown argument {a:?}"),
        ArgError::Missing(f) => format!("{f} wants a value"),
        ArgError::Bad(f) => format!("bad value for {f}"),
        ArgError::Twice(f) => format!("{f} given twice"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn preview_options_in_any_order_once_each() {
        let want = Cli::Preview {
            dir: "out".into(),
            scene: Some("t1_idle".into()),
            lang: PreviewLang::Pseudo,
            dpi: Some(120),
        };
        assert_eq!(
            parse(&a(&["--preview", "out", "--dpi", "120", "--lang", "pseudo", "--scene", "t1_idle"])),
            Ok(want)
        );
        assert!(parse(&a(&["--preview", "out", "--dpi", "120", "--dpi", "96"])).is_err());
        assert!(parse(&a(&["--preview", "out", "--dpi", "0"])).is_err());
        assert!(parse(&a(&["--preview", "out", "--lang"])).is_err());
        assert!(parse(&a(&["--preview", "out", "--scene", "../x"])).is_err());
        assert!(parse(&a(&["--preview"])).is_err());
        assert!(parse(&a(&["--preview", "--scene", "x"])).is_err());
    }

    #[test]
    fn modes_take_nothing_else() {
        assert!(parse(&a(&["--version", "--from-chord"])).is_err());
        assert!(parse(&a(&["--list-apps", "x"])).is_err());
        assert!(parse(&a(&["--selftest", "--signal", "ab", "x"])).is_err());
        assert!(parse(&a(&["--selftest", "--signal", "a b"])).is_err());
        assert!(parse(&a(&["--from-chord", "--version"])).is_err(), "a mode flag only comes first");
    }
}
