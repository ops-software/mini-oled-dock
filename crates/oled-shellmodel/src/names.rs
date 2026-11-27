//! The name of a program: the order of the sources (AppsFolder, Start Menu shortcut, version strings, exe stem, AUMID,
//! key) and the heuristics that pick and clean a name. Pure, except `SystemSources`, which reads the system.

use std::collections::BTreeMap;
use std::path::Path;

use crate::identity::{exe_file_name, is_usable_exe, norm_path};
use crate::version::FileInfo;

/// The step of `natural_name` that produced a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameStep {
    Aumid,
    Shortcut,
    Description,
    Stem,
    AumidId,
    Key,
}

impl NameStep {
    /// The word logs and `--list-apps` print for the step.
    pub fn as_str(self) -> &'static str {
        match self {
            NameStep::Aumid => "aumid",
            NameStep::Shortcut => "shortcut",
            NameStep::Description => "description",
            NameStep::Stem => "stem",
            NameStep::AumidId => "aumid-id",
            NameStep::Key => "key",
        }
    }
}

/// What a name is asked for: the grouping key, the exe and the AppUserModelID of a program.
#[derive(Clone, Copy, Debug)]
pub struct NameQuery<'a> {
    pub key: &'a str,
    pub exe_path: &'a str,
    pub aumid: Option<&'a str>,
}

/// The places a name can come from. The dock reads the Start Menu index and the agent; Tasks reads the system only.
pub trait NameSources {
    fn appsfolder_name(&self, aumid: &str) -> Option<String>;
    fn start_menu_name(&self, exe_path: &str) -> Option<String>;
    fn file_info(&self, exe_path: &str) -> crate::version::FileInfo;
    fn in_windows_dir(&self, exe_path: &str) -> bool;
}

/// AppsFolder, `FileInfo::read`, %SystemRoot%; no Start Menu index (`start_menu_name` is None): Tasks' sources.
pub struct SystemSources;

impl NameSources for SystemSources {
    fn appsfolder_name(&self, aumid: &str) -> Option<String> {
        crate::appsfolder::display_name(aumid)
    }

    fn start_menu_name(&self, _exe_path: &str) -> Option<String> {
        None
    }

    fn file_info(&self, exe_path: &str) -> FileInfo {
        FileInfo::read(exe_path)
    }

    fn in_windows_dir(&self, exe_path: &str) -> bool {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        in_windows_dir_of(exe_path, &root)
    }
}

/// True for executables under `system_root` (`%SystemRoot%`).
#[inline]
pub fn in_windows_dir_of(exe_path: &str, system_root: &str) -> bool {
    norm_path(exe_path).starts_with(&format!("{}\\", norm_path(system_root).trim_end_matches('\\')))
}

/// Override from `config.names`: key = AUMID, exe file name (lowercase) or group key,
/// case-insensitive comparison; blank values are ignored.
#[inline]
pub fn override_name(names: &BTreeMap<String, String>, q: &NameQuery<'_>) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let find = |k: &str| -> Option<String> {
        if k.is_empty() {
            return None;
        }
        names
            .iter()
            .find(|(nk, v)| nk.eq_ignore_ascii_case(k) && !v.trim().is_empty())
            .map(|(_, v)| v.trim().to_string())
    };
    q.aumid.and_then(find).or_else(|| exe_file_name(q.exe_path).and_then(|n| find(&n))).or_else(|| find(q.key))
}

#[inline]
pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App" -> "WindowsTerminal".
#[inline]
pub fn name_from_aumid(aumid: &str) -> String {
    let base = aumid.split(['!', '_']).next().unwrap_or(aumid);
    capitalize(base.rsplit('.').next().unwrap_or(base))
}

/// Program name without override, in this order: (b) AppsFolder catalog (packaged / explicit AUMID),
/// (c) Start Menu shortcut name for the exe, (d) smart choice between FileDescription and ProductName,
/// (e) prettified exe stem, then (last resort) AUMID piece or key. Also says which step produced it.
#[inline]
pub fn natural_name(q: &NameQuery<'_>, src: &impl NameSources) -> (String, NameStep) {
    let aumid = q.aumid.map(str::trim).filter(|a| !a.is_empty());
    if let Some(a) = aumid {
        if let Some(n) = src.appsfolder_name(a) {
            return (n, NameStep::Aumid);
        }
    }
    if is_usable_exe(q.exe_path) {
        if let Some(n) = src.start_menu_name(q.exe_path) {
            return (n, NameStep::Shortcut);
        }
        let stem = Path::new(q.exe_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let info = src.file_info(q.exe_path);
        // Windows' own programs have localized descriptions ("Esplora risorse") that share no word with the stem.
        if src.in_windows_dir(q.exe_path) {
            if let Some(d) = info.description.as_deref().map(strip_generic).filter(|d| !d.is_empty()) {
                return (d, NameStep::Description);
            }
        }
        if let Some(n) =
            pick_description(info.description.as_deref(), info.product.as_deref(), info.company.as_deref(), &stem)
        {
            return (n, NameStep::Description);
        }
        let pretty = pretty_stem(&stem);
        if !pretty.is_empty() {
            return (pretty, NameStep::Stem);
        }
    }
    if let Some(a) = aumid {
        let n = name_from_aumid(a);
        if !n.is_empty() {
            return (n, NameStep::AumidId);
        }
    }
    if let Some(stem) = Path::new(q.exe_path).file_stem().map(|s| s.to_string_lossy().into_owned()) {
        if !stem.is_empty() {
            return (capitalize(&stem), NameStep::Stem);
        }
    }
    (q.key.to_string(), NameStep::Key)
}

// ------------------------------------------------------------------ name heuristics (pure)

/// Words of `s`, case preserved: splits on non-alphanumerics and camelCase boundaries
/// (`RazerAppEngine` -> Razer App Engine, `HTTPServer` -> HTTP Server).
#[inline]
fn split_words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(cur.drain(..).collect());
            }
            continue;
        }
        if let Some(&prev) = cur.last() {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary =
                (prev.is_lowercase() && c.is_uppercase()) || (prev.is_uppercase() && c.is_uppercase() && next_lower);
            if boundary {
                out.push(cur.drain(..).collect());
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur.into_iter().collect());
    }
    out
}

#[inline]
fn tokens(s: &str) -> Vec<String> {
    split_words(s).into_iter().map(|w| w.to_lowercase()).collect()
}

/// Corporate suffixes ignored when matching the company name.
const COMPANY_NOISE: [&str; 10] =
    ["inc", "corporation", "corp", "ltd", "llc", "co", "gmbh", "software", "the", "limited"];

/// True if some token of `cand` equals (or, for tokens of 4+ chars, is a prefix of / prefixed by) a token of `refs`.
#[inline]
fn shares_token(cand: &[String], refs: &[String]) -> bool {
    cand.iter().any(|c| {
        refs.iter().any(|r| {
            c == r || (c.len() >= 4 && r.len() >= 4 && (r.starts_with(c.as_str()) || c.starts_with(r.as_str())))
        })
    })
}

/// Generic trailing words removed from a name (only while something remains).
const GENERIC_TAIL: [&str; 10] =
    ["client", "bootstrapper", "webhelper", "helper", "host", "launcher", "application", "app", "service", "desktop"];

#[inline]
pub fn strip_generic(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    while words.len() > 1 && words.last().is_some_and(|w| GENERIC_TAIL.contains(&w.to_lowercase().as_str())) {
        words.pop();
    }
    words.join(" ")
}

/// `PowerToys.QuickAccess`-style identifier: contains a dot between alphanumerics and no spaces.
#[inline]
fn is_dotted_identifier(s: &str) -> bool {
    !s.contains(char::is_whitespace) && s.contains('.')
}

/// True if `a` is a whole-word prefix of `b` (case-insensitive): "Steam" of "Steam Client".
#[inline]
fn word_prefix(a: &str, b: &str) -> bool {
    let (ta, tb) = (tokens(a), tokens(b));
    !ta.is_empty() && ta.len() < tb.len() && tb[..ta.len()] == ta[..]
}

/// Smart choice between FileDescription and ProductName: a candidate sharing no word with the other
/// sources (exe stem, the other candidate, company) is rejected (taglines like "Freedom to Stream"),
/// as are dotted identifiers. Generic tails are stripped; the shorter of two nested names wins.
#[inline]
pub fn pick_description(
    desc: Option<&str>,
    product: Option<&str>,
    company: Option<&str>,
    stem: &str,
) -> Option<String> {
    let company_tokens: Vec<String> =
        company.map(tokens).unwrap_or_default().into_iter().filter(|t| !COMPANY_NOISE.contains(&t.as_str())).collect();
    let valid = |c: Option<&str>, other: Option<&str>, with_company: bool| -> Option<String> {
        let c = c.map(str::trim).filter(|c| !c.is_empty() && !is_dotted_identifier(c))?;
        let mut refs = tokens(stem);
        refs.extend(other.map(tokens).unwrap_or_default());
        if with_company {
            refs.extend(company_tokens.iter().cloned());
        }
        if !shares_token(&tokens(c), &refs) {
            return None;
        }
        let s = strip_generic(c);
        Some(s).filter(|s| !s.is_empty())
    };
    let d = valid(desc, product, true);
    let p = valid(product, desc, false);
    match (d, p) {
        (Some(d), Some(p)) => Some(if word_prefix(&p, &d) { p } else { d }),
        (Some(d), None) => Some(d),
        (None, p) => p,
    }
}

/// Words dropped when prettifying an exe stem.
const STEM_NOISE: [&str; 9] = ["shell", "ng", "x64", "x86", "win64", "win32", "helper", "app", "exe"];

/// `stremio-shell-ng` -> "Stremio"; `RazerAppEngine` -> "Razer App Engine"; `zen` -> "Zen".
#[inline]
pub fn pretty_stem(stem: &str) -> String {
    let words = split_words(stem);
    let kept: Vec<&String> = words.iter().filter(|w| !STEM_NOISE.contains(&w.to_lowercase().as_str())).collect();
    let chosen: Vec<&String> = if kept.is_empty() { words.iter().collect() } else { kept };
    chosen
        .iter()
        .map(|w| if w.chars().any(char::is_uppercase) { (*w).clone() } else { capitalize(w) })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn words_and_tokens() {
        assert_eq!(split_words("RazerAppEngine"), ["Razer", "App", "Engine"]);
        assert_eq!(split_words("stremio-shell-ng"), ["stremio", "shell", "ng"]);
        assert_eq!(split_words("HTTPServer"), ["HTTP", "Server"]);
    }

    #[test]
    fn description_choice() {
        let pick = |d, p, c, s| pick_description(d, p, c, s);
        // Tagline rejected, nothing else usable.
        assert_eq!(
            pick(Some("Freedom to Stream"), Some("stremio-shell-ng"), None, "stremio-shell-ng"),
            Some("stremio-shell-ng".into())
        );
        // Dotted identifier loses against the product name.
        assert_eq!(
            pick(Some("PowerToys.QuickAccess"), Some("PowerToys"), None, "PowerToys.QuickAccess"),
            Some("PowerToys".into())
        );
        // Generic tails stripped, shorter nested name wins.
        assert_eq!(pick(Some("Steam Client WebHelper"), Some("Steam"), None, "steamwebhelper"), Some("Steam".into()));
        assert_eq!(
            pick(Some("Steam Client Bootstrapper"), Some("Steam"), Some("Valve"), "steam"),
            Some("Steam".into())
        );
        assert_eq!(pick(Some("Discord"), Some("Discord"), Some("Discord Inc."), "Discord"), Some("Discord".into()));
        assert_eq!(pick(Some("Freedom to Stream"), None, None, "stremio-shell-ng"), None);
    }

    #[test]
    fn stems() {
        assert_eq!(pretty_stem("stremio-shell-ng"), "Stremio");
        assert_eq!(pretty_stem("zen"), "Zen");
    }

    /// Made-up sources: what each program would have in the catalogue, the Start Menu and its version resource.
    #[derive(Default)]
    struct FakeSources {
        appsfolder: HashMap<&'static str, &'static str>,
        shortcuts: HashMap<&'static str, &'static str>,
        info: HashMap<&'static str, FileInfo>,
    }

    impl NameSources for FakeSources {
        fn appsfolder_name(&self, aumid: &str) -> Option<String> {
            self.appsfolder.get(aumid).map(|s| s.to_string())
        }
        fn start_menu_name(&self, exe_path: &str) -> Option<String> {
            self.shortcuts.get(exe_path).map(|s| s.to_string())
        }
        fn file_info(&self, exe_path: &str) -> FileInfo {
            self.info.get(exe_path).cloned().unwrap_or_default()
        }
        fn in_windows_dir(&self, exe_path: &str) -> bool {
            in_windows_dir_of(exe_path, r"C:\Windows")
        }
    }

    fn info(description: Option<&str>, product: Option<&str>, company: Option<&str>) -> FileInfo {
        let own = |s: Option<&str>| s.map(str::to_string);
        FileInfo { description: own(description), product: own(product), company: own(company) }
    }

    /// The fixture the Tasks app compares its names with: ten made-up programs, each reaching a different branch of
    /// `natural_name`, with the name and the step pinned.
    #[test]
    fn names_of_ten_fixture_apps() {
        const NOTES_ID: &str = "Contoso.Notes_8wekyb3d8bbwe!App";
        const DRAW: &str = r"C:\Program Files\Contoso\ctsdraw.exe";
        const CHAT: &str = r"C:\Program Files\Example Chat\chat.exe";
        const PLAYER: &str = r"C:\Program Files\Fabrikam\fabrikamplayer.exe";
        const STREAM: &str = r"C:\Program Files\Stream\stremio-shell-ng.exe";
        const TOYS: &str = r"C:\Program Files\PowerToys\PowerToys.QuickAccess.exe";
        const EXPLORER: &str = r"C:\Windows\explorer.exe";
        const ZEN: &str = r"C:\Tools\zen.exe";
        let mut src = FakeSources::default();
        src.appsfolder.insert(NOTES_ID, "Contoso Notes");
        src.shortcuts.insert(DRAW, "Contoso Draw");
        src.info.insert(CHAT, info(Some("Example Chat Client"), Some("Example Chat"), Some("Example Inc.")));
        src.info.insert(PLAYER, info(Some("Freedom to Stream"), Some("Fabrikam Player"), None));
        src.info.insert(STREAM, info(Some("Freedom to Stream"), None, None));
        src.info.insert(TOYS, info(Some("PowerToys.QuickAccess"), Some("PowerToys"), None));
        src.info.insert(EXPLORER, info(Some("Esplora risorse"), Some("Sistema operativo Microsoft Windows"), None));
        let ask = |key: &str, exe_path: &str, aumid: Option<&str>| {
            let (name, step) = natural_name(&NameQuery { key, exe_path, aumid }, &src);
            (name, step)
        };
        let table = [
            // A packaged app: the catalogue name.
            (ask(NOTES_ID, "", Some(NOTES_ID)), ("Contoso Notes", NameStep::Aumid)),
            // A program with a Start Menu shortcut.
            (ask(DRAW, DRAW, None), ("Contoso Draw", NameStep::Shortcut)),
            // Description and product agree: the description, its generic tail stripped.
            (ask(CHAT, CHAT, None), ("Example Chat", NameStep::Description)),
            // A tagline description is rejected: the product name stands.
            (ask(PLAYER, PLAYER, None), ("Fabrikam Player", NameStep::Description)),
            // A tagline and nothing else: the prettified stem.
            (ask(STREAM, STREAM, None), ("Stremio", NameStep::Stem)),
            // A dotted identifier loses against the product name.
            (ask(TOYS, TOYS, None), ("PowerToys", NameStep::Description)),
            // A Windows program: the localised description, whatever the stem.
            (ask(EXPLORER, EXPLORER, None), ("Esplora risorse", NameStep::Description)),
            // A bare stem.
            (ask(ZEN, ZEN, None), ("Zen", NameStep::Stem)),
            // An AUMID the catalogue does not know, no exe: the piece of the AUMID.
            (ask("Fabrikam.Calc_abc123!App", "", Some("Fabrikam.Calc_abc123!App")), ("Calc", NameStep::AumidId)),
            // Nothing at all: the key.
            (ask("hwnd:42", "", None), ("hwnd:42", NameStep::Key)),
        ];
        for (i, ((name, step), (want, want_step))) in table.iter().enumerate() {
            assert_eq!((name.as_str(), *step), (*want, *want_step), "app {i}");
        }
    }

    #[test]
    fn config_names_override_by_aumid_then_exe_then_key() {
        let names: BTreeMap<String, String> = [
            ("Contoso.Notes_8wekyb3d8bbwe!App", "  Notes  "),
            ("CTSDRAW.EXE", "Draw"),
            ("c:\\x\\blank.exe", "   "),
            ("c:\\x\\keyed.exe", "Keyed"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let q = |key, exe_path, aumid| NameQuery { key, exe_path, aumid };
        let aumid = Some("contoso.notes_8wekyb3d8bbwe!app");
        assert_eq!(override_name(&names, &q("k", r"C:\a\ctsdraw.exe", aumid)).as_deref(), Some("Notes"));
        assert_eq!(override_name(&names, &q("k", r"C:\a\ctsdraw.exe", None)).as_deref(), Some("Draw"));
        assert_eq!(override_name(&names, &q("c:\\x\\keyed.exe", r"C:\a\other.exe", None)).as_deref(), Some("Keyed"));
        assert_eq!(override_name(&names, &q("c:\\x\\blank.exe", r"C:\a\other.exe", None)), None, "blank ignored");
        assert_eq!(override_name(&BTreeMap::new(), &q("k", "", None)), None);
    }

    #[test]
    fn windows_dir_is_a_prefix_test_on_normalised_paths() {
        assert!(in_windows_dir_of(r"c:/windows/System32/cmd.exe", r"C:\Windows\"));
        assert!(!in_windows_dir_of(r"C:\WindowsApps\x.exe", r"C:\Windows"));
    }
}
