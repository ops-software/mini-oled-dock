//! files.exe's command line (P3-files-app 2.1). P1-files-engine's --role= arms run before it in main.rs. Every path
//! from the command line is decoded through WidePath and navigated, never executed (U5).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oled_trust::frame::files::{InstanceCmd, Location, WidePath};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, FILE_ATTRIBUTE_DIRECTORY, INVALID_FILE_ATTRIBUTES};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    /// forward_only: debug builds only (parse refuses it in release).
    Window {
        target: Option<String>,
        tab: bool,
        select: Option<String>,
        from_verb: bool,
        forward_only: bool,
    },
    Preview {
        dir: String,
        scene: Option<String>,
        lang: Option<String>,
        dpi: Option<u32>,
    },
    /// --selftest --signal <32 lower hex> (P2-plumbing F-P7).
    Selftest {
        nonce: String,
    },
}

/// The forms files.exe takes, for the usage line.
pub const USAGE: &str = "usage: files.exe [<folder or file>] [--tab <folder>] [--select <file>] [--from-verb] | \
                         files.exe --preview <existing folder> [--scene <name>] [--lang en|it] [--dpi 96|120|144|192] | \
                         files.exe --selftest --signal <32 lower hex>";

/// The DPIs a preview renders.
const DPIS: [u32; 4] = [96, 120, 144, 192];

fn usage(why: &str) -> String {
    format!("files: {why}; {USAGE}")
}

/// What the flags set, before the forms are told apart.
#[derive(Default)]
struct Seen {
    target: Option<String>,
    tab: bool,
    select: Option<String>,
    from_verb: bool,
    forward_only: bool,
    preview: Option<String>,
    scene: Option<String>,
    lang: Option<String>,
    dpi: Option<u32>,
    selftest: bool,
    signal: Option<String>,
}

/// Sets a value once; a second time is an error.
fn once<T>(slot: &mut Option<T>, v: T, flag: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(usage(&format!("{flag} given twice")));
    }
    *slot = Some(v);
    Ok(())
}

fn flag(on: &mut bool, name: &str) -> Result<(), String> {
    if *on {
        return Err(usage(&format!("{name} given twice")));
    }
    *on = true;
    Ok(())
}

fn is_nonce(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The arguments without argv[0]; Err = the one-line usage message (exit 2).
pub fn parse(args: &[String]) -> Result<Cli, String> {
    parse_in(args, cfg!(debug_assertions))
}

/// parse with the build's flavour explicit: --forward-only exists in debug builds only.
fn parse_in(args: &[String], debug: bool) -> Result<Cli, String> {
    let mut s = Seen::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| usage(&format!("{name} wants a value")));
        match a.as_str() {
            "--tab" => {
                flag(&mut s.tab, a)?;
                let v = value(a)?;
                once(&mut s.target, v, a)?;
            }
            "--select" => {
                let v = value(a)?;
                once(&mut s.select, v, a)?;
            }
            "--from-verb" => flag(&mut s.from_verb, a)?,
            "--forward-only" if debug => flag(&mut s.forward_only, a)?,
            "--preview" => {
                let v = value(a)?;
                once(&mut s.preview, v, a)?;
            }
            "--scene" => {
                let v = value(a)?;
                once(&mut s.scene, v, a)?;
            }
            "--lang" => {
                let v = value(a)?;
                if v != "en" && v != "it" {
                    return Err(usage("--lang wants en or it"));
                }
                once(&mut s.lang, v, a)?;
            }
            "--dpi" => {
                let v = value(a)?;
                let dpi = v.parse::<u32>().ok().filter(|d| DPIS.contains(d));
                let dpi = dpi.ok_or_else(|| usage("--dpi wants 96, 120, 144 or 192"))?;
                once(&mut s.dpi, dpi, a)?;
            }
            "--selftest" => flag(&mut s.selftest, a)?,
            "--signal" => {
                let v = value(a)?;
                if !is_nonce(&v) {
                    return Err(usage("--signal wants 32 lower-case hex digits"));
                }
                once(&mut s.signal, v, a)?;
            }
            other if other.starts_with("--") => return Err(usage(&format!("unknown flag {other}"))),
            other => {
                if s.target.is_some() {
                    return Err(usage("one target only"));
                }
                s.target = Some(other.to_string());
            }
        }
    }
    let window_flags = s.target.is_some() || s.tab || s.select.is_some() || s.from_verb || s.forward_only;
    let preview_flags = s.scene.is_some() || s.lang.is_some() || s.dpi.is_some();
    match (s.preview, s.selftest) {
        (Some(_), true) => Err(usage("--preview and --selftest are separate starts")),
        (Some(dir), false) => {
            if window_flags || s.signal.is_some() {
                return Err(usage("--preview takes only --scene, --lang and --dpi"));
            }
            Ok(Cli::Preview { dir, scene: s.scene, lang: s.lang, dpi: s.dpi })
        }
        (None, true) => {
            if window_flags || preview_flags {
                return Err(usage("--selftest takes only --signal"));
            }
            let nonce = s.signal.ok_or_else(|| usage("--selftest wants --signal"))?;
            Ok(Cli::Selftest { nonce })
        }
        (None, false) => {
            if preview_flags || s.signal.is_some() {
                return Err(usage("--scene, --lang, --dpi and --signal belong to --preview and --selftest"));
            }
            if s.target.is_some() && s.select.is_some() {
                return Err(usage("one target only"));
            }
            let (target, select) = (s.target, s.select);
            Ok(Cli::Window { target, tab: s.tab, select, from_verb: s.from_verb, forward_only: s.forward_only })
        }
    }
}

/// The command a window start forwards to a running instance: no target, Home; a folder, a new window on it (a tab in
/// the active window with --tab); a file, its folder with it selected; a target that does not exist, a new window on
/// it (it shows Not found). A relative target resolves against cwd. The target is stated with GetFileAttributesW on
/// its \\?\ form (a read, before any window).
pub fn to_instance(c: &Cli, cwd: &WidePath) -> Result<InstanceCmd, String> {
    to_instance_with(c, cwd, &stat)
}

/// to_instance over a given stat: Some(true) a folder, Some(false) a file, None nothing there.
pub fn to_instance_with(
    c: &Cli,
    cwd: &WidePath,
    stat: &dyn Fn(&WidePath) -> Option<bool>,
) -> Result<InstanceCmd, String> {
    let Cli::Window { target, tab, select, .. } = c else {
        return Err(usage("only a window start forwards"));
    };
    let Some(text) = target.as_ref().or(select.as_ref()) else {
        return Ok(InstanceCmd::NewWindow(Location::Home));
    };
    let path = resolve(text, cwd).ok_or_else(|| usage(&format!("not a path: {text}")))?;
    Ok(match (stat(&path), *tab) {
        (Some(false), _) => match path.parent() {
            Some(dir) => InstanceCmd::Select { dir, item: path },
            None => InstanceCmd::NewWindow(Location::Fs(path)),
        },
        (_, true) => InstanceCmd::OpenTab(Location::Fs(path)),
        (_, false) => InstanceCmd::NewWindow(Location::Fs(path)),
    })
}

/// An absolute path as given (any form WidePath takes), or one whose "." and ".." components walk, never above the
/// root: an absolute path with such components from its own root ("X:\" or "\\server\share"), a path rooted without
/// a drive ("\x") from the root of cwd, a relative one from cwd. A drive-relative form ("C:x") is refused: its folder
/// is that drive's own current folder, which is not known here. So are device forms ("\\?\", "\\.\") that WidePath
/// refused.
fn resolve(text: &str, cwd: &WidePath) -> Option<WidePath> {
    let units: Vec<u16> = text.trim().encode_utf16().collect();
    if let Ok(p) = WidePath::new(&units) {
        return Some(p);
    }
    let sep = |u: u16| u == u16::from(b'\\') || u == u16::from(b'/');
    let drive = units.get(1) == Some(&u16::from(b':'));
    let drive_root = drive && units.get(2).is_some_and(|&u| sep(u));
    let unc = units.len() >= 2 && units.iter().take(2).all(|&u| sep(u));
    if units.is_empty() || (drive && !drive_root) {
        return None;
    }
    let (mut base, rest): (WidePath, &[u16]) = if drive_root {
        (WidePath::new(units.get(..3)?).ok()?, units.get(3..)?)
    } else if unc {
        unc_root(&units, sep)?
    } else if units.first().is_some_and(|&u| sep(u)) {
        (root_of(cwd), units.as_slice())
    } else {
        (cwd.clone(), units.as_slice())
    };
    for comp in rest.split(|&u| sep(u)).filter(|c| !c.is_empty()) {
        match comp {
            [d] if *d == u16::from(b'.') => {}
            [d, e] if *d == u16::from(b'.') && *e == u16::from(b'.') => {
                if let Some(up) = base.parent() {
                    base = up;
                }
            }
            name => base = base.join(name).ok()?,
        }
    }
    Some(base)
}

/// "\\server\share" of a UNC text and the units after it; None when the server or the share is missing, or WidePath
/// refuses the root (a device form such as "\\?\" or "\\.\").
fn unc_root(units: &[u16], sep: impl Fn(u16) -> bool) -> Option<(WidePath, &[u16])> {
    let body = units.get(2..)?;
    let mut seps = body.iter().enumerate().filter(|&(_, &u)| sep(u)).map(|(i, _)| i);
    seps.next()?; // the end of the server
    let end = 2 + seps.next().unwrap_or(body.len());
    Some((WidePath::new(units.get(..end)?).ok()?, units.get(end..)?))
}

/// The drive or share root of a path.
fn root_of(p: &WidePath) -> WidePath {
    let mut r = p.clone();
    while let Some(up) = r.parent() {
        r = up;
    }
    r
}

/// GetFileAttributesW on the \\?\ form: Some(is_dir), None when nothing is there.
fn stat(p: &WidePath) -> Option<bool> {
    let raw = p.raw();
    // SAFETY: raw is NUL-terminated and lives for the call.
    let a = unsafe { GetFileAttributesW(PCWSTR(raw.as_ptr())) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a & FILE_ATTRIBUTE_DIRECTORY.0 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn forward_only_follows_the_build() {
        let a = args(&["--forward-only", r"C:\x"]);
        let window = |forward_only| Cli::Window {
            target: Some(r"C:\x".into()),
            tab: false,
            select: None,
            from_verb: false,
            forward_only,
        };
        assert_eq!(parse_in(&a, true), Ok(window(true)));
        assert!(parse_in(&a, false).is_err(), "a release build refuses it (exit 2)");
    }
}
