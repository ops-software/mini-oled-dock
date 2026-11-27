//! The gallery's command line (P2-kit K0.2): a pure parser, so every line of the live sessions is tested.
//!
//! Modes: no argument (the gallery window), `--page <name> [--caption custom|system] [--focus-row <n>]`,
//! `--preview <dir> [--lang en|it|pseudo] [--dpi <n>] [--page <name>]`, `--measure <mode> [options]`, `--list-monitors`,
//! `--bench [<name>]`. A measure option is accepted only by the modes that read it (MEASURE_FLAGS).

use std::path::PathBuf;

use crate::measure::Mode;

/// One line of usage, printed with a parse error.
pub const USAGE: &str = "usage: gallery [--page <name> [--caption custom|system] [--focus-row <n>]] | --preview <dir> \
[--lang en|it|pseudo] [--dpi <n>] [--page <name>] | --measure empty|idle|leak|theme|band|anim|caption [options] | \
--list-monitors | --bench [<name>]";

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    /// The gallery window, or one page.
    Window(WindowOpts),
    Preview(PreviewOpts),
    /// A measure mode (--list-monitors is Mode::Monitors).
    Measure(MeasureOpts),
    /// Every ready bench, or the one named.
    Bench(Option<String>),
}

/// The caption a --page window asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionArg {
    Custom,
    System,
}

/// The window mode's options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowOpts {
    pub page: Option<String>,
    pub caption: Option<CaptionArg>,
    pub focus_row: Option<usize>,
}

/// A preview's language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewLang {
    En,
    It,
    Pseudo,
}

/// --preview's options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewOpts {
    pub dir: PathBuf,
    pub lang: Option<PreviewLang>,
    pub dpi: Option<u32>,
    pub page: Option<String>,
}

/// The frame pacing an anim run uses (K3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    A,
    B,
}

/// --measure's options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeasureOpts {
    pub mode: Mode,
    pub page: Option<String>,
    pub secs: Option<u32>,
    pub monitor: Option<u32>,
    pub size: Option<(u32, u32)>,
    pub pacing: Option<Pacing>,
    pub ole: bool,
    pub no_text: bool,
    pub no_window: bool,
    pub cycles: Option<u32>,
    pub out: Option<PathBuf>,
}

impl MeasureOpts {
    /// A mode with no option.
    pub fn new(mode: Mode) -> MeasureOpts {
        MeasureOpts {
            mode,
            page: None,
            secs: None,
            monitor: None,
            size: None,
            pacing: None,
            ole: false,
            no_text: false,
            no_window: false,
            cycles: None,
            out: None,
        }
    }
}

/// The options each measure mode reads (P2-kit K1a.7, K1a.10, K1a.11, K2a.11, K3.4, K3.5).
pub const MEASURE_FLAGS: [(Mode, &[&str]); 8] = [
    (Mode::Empty, &["--secs", "--ole", "--no-text", "--no-window", "--out"]),
    (Mode::Idle, &["--page", "--secs"]),
    (Mode::Leak, &["--cycles"]),
    (Mode::Theme, &[]),
    (Mode::Band, &["--monitor", "--secs", "--size"]),
    (Mode::Anim, &["--monitor", "--pacing"]),
    (Mode::Caption, &[]),
    (Mode::Monitors, &[]),
];

/// Flags followed by a value.
const VALUED: [&str; 13] = [
    "--page",
    "--caption",
    "--focus-row",
    "--preview",
    "--lang",
    "--dpi",
    "--measure",
    "--secs",
    "--monitor",
    "--size",
    "--pacing",
    "--cycles",
    "--out",
];

/// Flags without a value (--bench takes an optional one).
const SWITCHES: [&str; 5] = ["--ole", "--no-text", "--no-window", "--list-monitors", "--bench"];

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if seen.iter().any(|(f, _)| *f == a) {
            return Err(format!("{a} is given twice"));
        }
        if VALUED.contains(&a) {
            let Some(v) = args.get(i + 1).filter(|v| !v.starts_with("--")) else {
                return Err(format!("{a} needs a value"));
            };
            seen.push((a, Some(v.as_str())));
            i += 2;
        } else if a == "--bench" {
            let v = args.get(i + 1).filter(|v| !v.starts_with("--")).map(String::as_str);
            seen.push((a, v));
            i += if v.is_some() { 2 } else { 1 };
        } else if SWITCHES.contains(&a) {
            seen.push((a, None));
            i += 1;
        } else {
            return Err(format!("unknown argument {a:?}"));
        }
    }
    let has = |f: &str| seen.iter().any(|(g, _)| *g == f);
    let value = |f: &str| seen.iter().find(|(g, _)| *g == f).and_then(|(_, v)| *v);
    let modes: Vec<&str> =
        ["--preview", "--measure", "--list-monitors", "--bench"].into_iter().filter(|m| has(m)).collect();
    if modes.len() > 1 {
        return Err(format!("{} cannot be combined", modes.join(" and ")));
    }
    let only = |allowed: &[&str], mode: &str| -> Result<(), String> {
        match seen.iter().find(|(f, _)| !allowed.contains(f)) {
            Some((f, _)) => Err(format!("{f} is not an option of {mode}")),
            None => Ok(()),
        }
    };
    let page = value("--page").map(str::to_string);
    match modes.first().copied() {
        None => {
            only(&["--page", "--caption", "--focus-row"], "the window")?;
            let caption = match value("--caption") {
                None => None,
                Some("custom") => Some(CaptionArg::Custom),
                Some("system") => Some(CaptionArg::System),
                Some(o) => return Err(format!("--caption {o:?}: custom or system")),
            };
            let focus_row = value("--focus-row").map(|v| number::<usize>("--focus-row", v)).transpose()?;
            Ok(Cli::Window(WindowOpts { page, caption, focus_row }))
        }
        Some("--preview") => {
            only(&["--preview", "--lang", "--dpi", "--page"], "--preview")?;
            let lang = match value("--lang") {
                None => None,
                Some("en") => Some(PreviewLang::En),
                Some("it") => Some(PreviewLang::It),
                Some("pseudo") => Some(PreviewLang::Pseudo),
                Some(o) => return Err(format!("--lang {o:?}: en, it or pseudo")),
            };
            let dpi = value("--dpi").map(|v| number::<u32>("--dpi", v)).transpose()?;
            if dpi.is_some_and(|d| !(48..=960).contains(&d)) {
                return Err("--dpi: 48 to 960".to_string());
            }
            let dir = PathBuf::from(value("--preview").unwrap_or_default());
            Ok(Cli::Preview(PreviewOpts { dir, lang, dpi, page }))
        }
        Some("--bench") => {
            only(&["--bench"], "--bench")?;
            Ok(Cli::Bench(value("--bench").map(str::to_string)))
        }
        Some(flag) => {
            let mode = if flag == "--list-monitors" {
                Mode::Monitors
            } else {
                let name = value("--measure").unwrap_or_default();
                Mode::from_name(name).ok_or_else(|| format!("--measure {name:?}: not a mode"))?
            };
            let allowed = MEASURE_FLAGS.iter().find(|(m, _)| *m == mode).map(|(_, f)| *f).unwrap_or(&[]);
            let mut ok: Vec<&str> = allowed.to_vec();
            ok.push(flag);
            only(&ok, &format!("--measure {}", mode.name()))?;
            let mut o = MeasureOpts::new(mode);
            o.page = page;
            o.secs = value("--secs").map(|v| positive("--secs", v)).transpose()?;
            o.monitor = value("--monitor").map(|v| number::<u32>("--monitor", v)).transpose()?;
            o.cycles = value("--cycles").map(|v| positive("--cycles", v)).transpose()?;
            o.size = value("--size").map(size).transpose()?;
            o.pacing = match value("--pacing") {
                None => None,
                Some("a") => Some(Pacing::A),
                Some("b") => Some(Pacing::B),
                Some(p) => return Err(format!("--pacing {p:?}: a or b")),
            };
            o.ole = has("--ole");
            o.no_text = has("--no-text");
            o.no_window = has("--no-window");
            o.out = value("--out").map(PathBuf::from);
            Ok(Cli::Measure(o))
        }
    }
}

fn number<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse::<T>().map_err(|_| format!("{flag} {v:?}: not a number"))
}

fn positive(flag: &str, v: &str) -> Result<u32, String> {
    match number::<u32>(flag, v)? {
        0 => Err(format!("{flag}: at least 1")),
        n => Ok(n),
    }
}

/// "<w>x<h>", both at least 1.
fn size(v: &str) -> Result<(u32, u32), String> {
    let bad = || format!("--size {v:?}: <w>x<h>");
    let (w, h) = v.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<Cli, String> {
        let args: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        parse(&args)
    }

    fn measure(line: &str) -> MeasureOpts {
        match p(line) {
            Ok(Cli::Measure(o)) => o,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_every_mode() {
        assert_eq!(p(""), Ok(Cli::Window(WindowOpts::default())));
        assert_eq!(
            p("--page table100k --caption custom --focus-row 512"),
            Ok(Cli::Window(WindowOpts {
                page: Some("table100k".into()),
                caption: Some(CaptionArg::Custom),
                focus_row: Some(512)
            }))
        );
        assert_eq!(
            p("--preview out --lang pseudo --dpi 144 --page buttons"),
            Ok(Cli::Preview(PreviewOpts {
                dir: PathBuf::from("out"),
                lang: Some(PreviewLang::Pseudo),
                dpi: Some(144),
                page: Some("buttons".into())
            }))
        );
        let o = measure("--measure empty --ole --no-text --no-window --secs 3 --out e.json");
        assert_eq!((o.mode, o.secs, o.ole, o.no_text, o.no_window), (Mode::Empty, Some(3), true, true, true));
        assert_eq!(o.out, Some(PathBuf::from("e.json")));
        assert_eq!(measure("--measure idle --page buttons --secs 60").page.as_deref(), Some("buttons"));
        assert_eq!(measure("--measure leak --cycles 20").cycles, Some(20));
        assert_eq!(measure("--measure theme").mode, Mode::Theme);
        let o = measure("--measure band --monitor 1 --secs 60 --size 640x560");
        assert_eq!((o.mode, o.monitor, o.size), (Mode::Band, Some(1), Some((640, 560))));
        let o = measure("--measure anim --monitor 0 --pacing a");
        assert_eq!((o.mode, o.pacing), (Mode::Anim, Some(Pacing::A)));
        assert_eq!(measure("--measure caption").mode, Mode::Caption);
        assert_eq!(measure("--list-monitors"), MeasureOpts::new(Mode::Monitors));
        assert_eq!(p("--bench"), Ok(Cli::Bench(None)));
        assert_eq!(p("--bench table_hover"), Ok(Cli::Bench(Some("table_hover".into()))));
    }

    #[test]
    fn cli_refuses_unknown_flags() {
        for bad in [
            "--nope",
            "buttons",
            "--page",
            "--measure",
            "--measure fast",
            "--caption sideways",
            "--focus-row x",
            "--page a --page b",
            "--preview out --lang fr",
            "--preview out --dpi 10",
            "--measure band --size 640",
            "--measure band --size 0x5",
            "--measure leak --cycles 0",
            "--measure anim --pacing c",
            "--bench a b",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cli_refuses_flags_of_other_modes() {
        for bad in [
            "--measure leak --pacing a",
            "--measure empty --page buttons",
            "--measure theme --secs 5",
            "--measure caption --monitor 0",
            "--measure idle --ole",
            "--list-monitors --secs 1",
            "--bench --page x",
            "--preview out --secs 1",
            "--page buttons --ole",
            "--preview out --measure idle",
            "--bench --list-monitors",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    /// Every command line of P2-kit section 5 (and the task lines it builds on), exactly as written there.
    #[test]
    fn cli_accepts_every_live_line() {
        for line in [
            "--measure empty --secs 60",
            "--measure empty --secs 60 --ole",
            "--measure empty --no-text",
            "--measure empty --no-window",
            "--measure empty --secs 1",
            "--page buttons --measure idle --secs 60",
            "--page buttons --measure idle --secs 10",
            "--measure leak --cycles 20",
            "",
            "--measure theme",
            "--bench",
            "--list-monitors",
            "--measure band --size 640x560",
            "--measure band --monitor 0 --secs 60",
            "--page menus",
            "--page table100k --focus-row 512",
            "--page fields",
            "--page buttons",
            "--measure anim --monitor 0 --pacing a",
            "--measure anim --monitor 0 --pacing b",
            "--measure caption",
        ] {
            assert!(p(line).is_ok(), "{line}: {:?}", p(line));
        }
    }
}
