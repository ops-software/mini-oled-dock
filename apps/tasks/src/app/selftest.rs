//! --selftest --signal <nonce> (P2-plumbing F-P7): before any mutex, no window, no forward, within 10 s (TS1).

use std::io::Read as _;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use oled_config::{Options as _, Problem};
use oled_ui::harness::Harness;
use oled_ui::{SizeL, Tokens, UiLang};
use windows::Win32::UI::Shell::FOLDERID_RoamingAppData;

use crate::app::exec::RecordingExecutor;
use crate::app::launch::RecordingLauncher;
use crate::app::options::{self, TasksOptions};
use crate::app::Deps;
use crate::strings::{app_name, TABLE};
use crate::ui::screen::TasksScreen;

/// The watchdog: the check must finish well inside the caller's 10 s.
const WATCHDOG: Duration = Duration::from_secs(9);
/// The most of theme.json the check reads (tasks.json uses the Store's own cap).
const READ_CAP: u64 = 1 << 20;

/// What the self-test looks at, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Strings,
    Render,
    Options,
    Theme,
}

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Check::Strings => "strings",
            Check::Render => "render",
            Check::Options => "options",
            Check::Theme => "theme",
        }
    }
}

/// The four parts, each true when sound; fakes in the tests, the real ones in selftest().
pub trait Parts {
    fn strings(&self) -> bool;
    fn render(&self) -> bool;
    fn options(&self) -> bool;
    fn theme(&self) -> bool;
}

/// The first part that is not sound.
pub fn first_failure(p: &dyn Parts) -> Option<Check> {
    [
        (Check::Strings, p.strings()),
        (Check::Render, p.render()),
        (Check::Options, p.options()),
        (Check::Theme, p.theme()),
    ]
    .into_iter()
    .find_map(|(c, ok)| (!ok).then_some(c))
}

/// Every key of the table has an English and an Italian text, and the display name resolves in both.
fn strings_sound() -> bool {
    let t = &TABLE;
    let n = t.hashes.len();
    let spans_full = |s: &oled_sys::i18n::Strings| s.spans.len() == n && s.spans.iter().all(|&(_, len)| len > 0);
    let name = |italian| !app_name(UiLang { italian, pseudo: false }).as_ref().is_empty();
    n > 0 && spans_full(&t.en) && spans_full(&t.it) && name(false) && name(true)
}

/// One headless render of the empty window at 96 DPI has a pixel that is not black.
fn render_sound() -> bool {
    let deps = Deps::inert(UiLang::EN, Arc::new(RecordingLauncher::default()), Arc::new(RecordingExecutor::default()));
    let mut h = Harness::new(TasksScreen::new(deps), SizeL { w: 640, h: 560 }, 96, Tokens::oled());
    h.render().px.iter().any(|p| p & 0x00FF_FFFF != 0)
}

/// The file's text: Ok(None) when it does not exist, Err when it cannot be read whole or is not text.
fn read_text(path: &std::path::Path, cap: u64) -> Result<Option<String>, ()> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut bytes = Vec::new();
    f.take(cap + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > cap {
        return Err(());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| ())
}

/// The text of tasks.json is sound when it is a JSON object: a wrong-typed or unknown key keeps its default in the app
/// and is no reason to call the image broken (the theme check is as lenient).
fn options_text_sound(text: &str) -> bool {
    !options::parse_text(text).1.iter().any(|e| matches!(e.problem, Problem::Parse(_)))
}

/// tasks.json is readable (within the Store's cap) and a JSON object (absent means defaults); nothing is created.
fn options_sound() -> bool {
    let Some(dir) = oled_sys::datadir::known_folder(&FOLDERID_RoamingAppData) else {
        return true;
    };
    match read_text(&dir.join("mini-oled-dock").join(TasksOptions::FILE), options::FILE_CAP) {
        Ok(None) => true,
        Ok(Some(text)) => options_text_sound(&text),
        Err(()) => false,
    }
}

/// theme.json parses (absent means defaults).
fn theme_sound() -> bool {
    let Some(path) = oled_config::theme::theme_path() else {
        return true;
    };
    match read_text(&path, READ_CAP) {
        Ok(None) => true,
        Ok(Some(text)) => oled_config::theme::parse(text.as_bytes()).is_ok(),
        Err(()) => false,
    }
}

struct Real;

impl Parts for Real {
    fn strings(&self) -> bool {
        strings_sound()
    }
    fn render(&self) -> bool {
        render_sound()
    }
    fn options(&self) -> bool {
        options_sound()
    }
    fn theme(&self) -> bool {
        theme_sound()
    }
}

/// --selftest: no mutex, no window, no forward, no network. All four parts sound: signal the nonce's event and exit 0;
/// otherwise one log line naming the part and exit 1. A watchdog exits 1 at 9 s.
pub fn run(nonce: &str) -> ExitCode {
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        oled_sys::log!("tasks: selftest: timed out");
        std::process::exit(1);
    });
    if let Some(bad) = first_failure(&Real) {
        oled_sys::log!("tasks: selftest: {} failed", bad.name());
        return ExitCode::from(1);
    }
    match oled_sys::selftest::signal("tasks", "selftest", nonce) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            oled_sys::log!("tasks: selftest: no signal ({:?})", e.kind());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake([bool; 4]);

    impl Parts for Fake {
        fn strings(&self) -> bool {
            self.0[0]
        }
        fn render(&self) -> bool {
            self.0[1]
        }
        fn options(&self) -> bool {
            self.0[2]
        }
        fn theme(&self) -> bool {
            self.0[3]
        }
    }

    #[test]
    fn selftest_checks_each_part() {
        assert_eq!(first_failure(&Fake([true; 4])), None);
        for (i, c) in [Check::Strings, Check::Render, Check::Options, Check::Theme].into_iter().enumerate() {
            let mut ok = [true; 4];
            ok[i] = false;
            assert_eq!(first_failure(&Fake(ok)), Some(c), "part {i}");
        }
        // The first failing part is the one named.
        assert_eq!(first_failure(&Fake([true, false, false, true])), Some(Check::Render));
    }

    #[test]
    fn the_real_parts_are_sound_here() {
        assert!(strings_sound(), "every key has both languages");
        assert!(render_sound(), "the empty window is not black");
    }

    #[test]
    fn options_check_fails_only_on_a_broken_file() {
        assert!(options_text_sound("{}"));
        assert!(options_text_sound(r#"{"unknown_key": 1}"#), "unknown key is fine");
        // A wrong-typed value or an unknown choice keeps the default: the app starts, so the self-test passes.
        let (_, errs) = options::parse_text(r#"{"refresh": 2, "always_on_top": "yes"}"#);
        assert!(!errs.is_empty(), "the fixture really has per-key errors");
        assert!(options_text_sound(r#"{"refresh": 2, "always_on_top": "yes"}"#));
        assert!(!options_text_sound("{"), "broken JSON fails");
        assert!(!options_text_sound("[1]"), "not an object fails");
    }

    #[test]
    fn a_file_over_the_cap_or_not_text_is_refused() {
        let dir = std::env::temp_dir().join(format!("tasks-selftest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cap = READ_CAP;
        assert_eq!(read_text(&dir.join("absent.json"), cap), Ok(None));
        std::fs::write(dir.join("ok.json"), "{}").unwrap();
        assert_eq!(read_text(&dir.join("ok.json"), cap), Ok(Some("{}".to_string())));
        std::fs::write(dir.join("binary.json"), [0xFFu8, 0xFE]).unwrap();
        assert_eq!(read_text(&dir.join("binary.json"), cap), Err(()));
        std::fs::write(dir.join("big.json"), vec![b' '; READ_CAP as usize + 1]).unwrap();
        assert_eq!(read_text(&dir.join("big.json"), cap), Err(()));
        // The options cap is the Store's: a file over it is refused although under the theme cap.
        std::fs::write(dir.join("mid.json"), vec![b' '; options::FILE_CAP as usize + 1]).unwrap();
        assert_eq!(read_text(&dir.join("mid.json"), options::FILE_CAP), Err(()));
        assert_eq!(read_text(&dir.join("mid.json"), cap), Ok(Some(" ".repeat(options::FILE_CAP as usize + 1))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
