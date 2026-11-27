//! What the user typed in the path field (P3-files-app 2.2; FD-PATH fills it, F4.4). Pure: Env supplies the
//! variables, the profile folder and the known folders; a Stat job decides folder or file afterwards.

use crate::handoff::Target;
use crate::listing::WidePath;
use crate::msg::KnownName;
use oled_trust::frame::files::PathError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// An absolute path (a drive, UNC or \\?\ form, or a relative one resolved against the current folder).
    Path(WidePath),
    /// shell:<known name>, resolved through the cached known folders.
    Shell(KnownName),
    /// A location File Explorer opens (the Recycle Bin, Network, This PC, other shell: names, a server alone).
    Virtual(Target),
    /// Text that is not a path: run only through ShellReq::RunTyped, from the user's own Enter.
    Command(String),
    /// The reason it is not a path: a short code ("empty", "control", "too_long", "not_absolute", "device"); the
    /// window words it through the string table.
    Invalid(String),
}

/// The environment parse reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    pub vars: Vec<(String, String)>,
    pub profile: Option<WidePath>,
    pub known: Vec<(KnownName, WidePath)>,
}

/// The shell: names that open a known folder of ours, and those File Explorer opens.
pub const SHELL_KNOWN: [&str; 7] = ["downloads", "documents", "desktop", "pictures", "music", "videos", "profile"];
pub const SHELL_VIRTUAL: [&str; 3] = ["recyclebinfolder", "networkplacesfolder", "mycomputerfolder"];

fn invalid(code: &str) -> Parsed {
    Parsed::Invalid(code.to_string())
}

fn code(e: PathError) -> &'static str {
    match e {
        PathError::Empty => "empty",
        PathError::TooLong => "too_long",
        PathError::Control => "control",
        PathError::NotAbsolute => "not_absolute",
        PathError::Device => "device",
        PathError::DotComponent => "dot",
    }
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// "~" and "%NAME%" expanded; an unknown variable stays as typed.
fn expand(t: &str, env: &Env) -> String {
    let mut s = t.to_string();
    if let Some(profile) = &env.profile {
        if s == "~" || s.starts_with("~\\") || s.starts_with("~/") {
            s = format!("{}{}", profile.to_string_lossy(), &s[1..]);
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('%').and_then(|close| {
            let name = &after[..close];
            let hit = env.vars.iter().find(|(n, _)| !name.is_empty() && n.eq_ignore_ascii_case(name));
            hit.map(|(_, v)| (close, v.as_str()))
        });
        match value {
            Some((close, v)) => {
                out.push_str(v);
                rest = &after[close + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn shell(text: &str, name: &str) -> Parsed {
    let name = name.to_ascii_lowercase();
    let known = match name.as_str() {
        "downloads" => Some(KnownName::Downloads),
        "documents" => Some(KnownName::Documents),
        "desktop" => Some(KnownName::Desktop),
        "pictures" => Some(KnownName::Pictures),
        "music" => Some(KnownName::Music),
        "videos" => Some(KnownName::Videos),
        "profile" => Some(KnownName::Profile),
        _ => None,
    };
    match (known, name.as_str()) {
        (Some(k), _) => Parsed::Shell(k),
        (None, "recyclebinfolder") => Parsed::Virtual(Target::RecycleBin),
        (None, "networkplacesfolder") => Parsed::Virtual(Target::Network),
        (None, "mycomputerfolder") => Parsed::Virtual(Target::ThisPc),
        _ => Parsed::Virtual(Target::Other(text.to_string())),
    }
}

/// Dot components resolved and repeated separators folded; the first "protected" components (the drive, or the
/// server and share) are never popped.
fn squash(full: &str) -> Result<String, &'static str> {
    let (prefix, rest, protected) = if starts_ci(full, r"\\?\UNC\") {
        (r"\\", &full[8..], 2)
    } else if starts_ci(full, r"\\?\") {
        let r = &full[4..];
        if starts_ci(r, "GLOBALROOT") || starts_ci(r, "Volume{") {
            return Err("device");
        }
        ("", r, 1)
    } else if full.starts_with(r"\\.") {
        return Err("device");
    } else if let Some(r) = full.strip_prefix(r"\\") {
        (r"\\", r, 2)
    } else {
        ("", full, 1)
    };
    let mut out: Vec<&str> = Vec::new();
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                if out.len() > protected {
                    out.pop();
                }
            }
            c => out.push(c),
        }
    }
    let mut s = format!("{prefix}{}", out.join("\\"));
    if prefix.is_empty() && out.len() == 1 {
        s.push('\\');
    }
    Ok(s)
}

fn root_of(cwd: &WidePath) -> String {
    let mut p = cwd.clone();
    while let Some(up) = p.parent() {
        p = up;
    }
    p.to_string_lossy().trim_end_matches('\\').to_string()
}

fn path(full: &str) -> Parsed {
    match squash(full) {
        Ok(s) => match WidePath::parse(&s) {
            Ok(p) => Parsed::Path(p),
            Err(e) => invalid(code(e)),
        },
        Err(c) => invalid(c),
    }
}

/// The text of the path field as a location, a command or a refusal. A drive, UNC or \\?\ path, a rooted or
/// relative one (against cwd), "~", "%VAR%" and "shell:" names; text with no separator, colon or dot name is a
/// Command (the caller first looks for a child of that name).
pub fn parse(input: &str, cwd: &WidePath, env: &Env) -> Parsed {
    let mut t = input.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t = t[1..t.len() - 1].trim();
    }
    if t.is_empty() {
        return invalid("empty");
    }
    let text = expand(t, env);
    if text.chars().any(|c| (c as u32) < 0x20) {
        return invalid("control");
    }
    if starts_ci(&text, "shell:") {
        return shell(&text, text[6..].trim());
    }
    let s = text.replace('/', "\\");
    let b = s.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\');
    if s.starts_with(r"\\") {
        let device = starts_ci(&s, r"\\.") || starts_ci(&s, r"\\?\GLOBALROOT") || starts_ci(&s, r"\\?\Volume{");
        let unc = starts_ci(&s, r"\\?\UNC\");
        let body = if unc { &s[8..] } else { s.strip_prefix(r"\\").unwrap_or(&s) };
        let plain = !device && !starts_ci(&s, r"\\?\");
        if (plain || unc) && body.split('\\').filter(|c| !c.is_empty()).count() < 2 {
            if body.chars().all(|c| c == '\\') {
                return invalid("not_absolute");
            }
            return Parsed::Virtual(Target::Other(t.to_string()));
        }
        return path(&s);
    }
    if drive {
        return path(&if b.len() == 2 { format!("{s}\\") } else { s });
    }
    if s.starts_with('\\') {
        return path(&format!("{}{s}", root_of(cwd)));
    }
    if s.contains(':') {
        return invalid("not_absolute");
    }
    if s.contains('\\') || s == "." || s == ".." {
        return path(&format!("{}\\{s}", cwd.to_string_lossy()));
    }
    Parsed::Command(t.to_string())
}
