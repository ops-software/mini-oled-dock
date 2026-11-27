//! files.exe --selftest --signal <nonce> (P3-files-app 2.2, F1a.18; P2-plumbing F-P7): the suite's updater asks a fresh
//! image whether it can start. It runs before any mutex, hand-off or window, whatever the elevation, forwards nothing,
//! and writes nothing but its log. All four parts sound: the nonce's event is signalled and the exit code is 0;
//! otherwise one log line names the part and the exit code is 1. A watchdog exits 1 before the caller's 10 s end.

use std::io::Read as _;
use std::time::Duration;

use oled_ui::UiLang;

use crate::fakes::Fakes;
use crate::options;
use crate::preview;
use crate::tr::gen::TABLE;

/// The watchdog: the checks must finish well inside the caller's 10 s.
const WATCHDOG: Duration = Duration::from_secs(9);
/// The most of theme.json the check reads.
const THEME_CAP: u64 = 1 << 20;
/// The most of files.json the check reads (the Store's own cap).
const OPTIONS_CAP: u64 = 64 * 1024;

/// The test hooks of the selftest tests, a comma list in this variable, read by debug builds only (`hook`). "drop-it"
/// blanks one Italian text for the strings check; "pause" stops after the checks (printing "paused") until
/// FILES_TEST_PARENT exits. Declared in every build so the tests compile in release, where it is never read.
pub const HOOK_VAR: &str = "FILES_TEST_SELFTEST";

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

/// The four parts, each true when sound; fakes in the tests, the real ones in run().
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

/// Whether the debug hook named `name` is set (always false in a release build).
fn hook(name: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        std::env::var(HOOK_VAR).is_ok_and(|v| v.split(',').any(|h| h == name))
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = name;
        false
    }
}

/// Every key has an English and an Italian text (`blank_it`: that Italian text is taken as missing, for the hook), and
/// the display name resolves in both languages.
fn strings_sound(blank_it: Option<usize>) -> bool {
    let n = TABLE.hashes.len();
    let full = |s: &oled_sys::i18n::Strings, blank: Option<usize>| {
        s.spans.len() == n && s.spans.iter().enumerate().all(|(i, &(_, len))| len > 0 && blank != Some(i))
    };
    let name = |italian| !UiLang { italian, pseudo: false }.kit(oled_ui::strings::k::KIT_APP_FILES).is_empty();
    n > 0 && full(&TABLE.en, None) && full(&TABLE.it, blank_it) && name(false) && name(true)
}

/// One render of the "details_basic" scene at 96 DPI, over the fakes, has a pixel that is not black.
fn render_sound() -> bool {
    let scene = preview::scenes().into_iter().find(|s| s.name == "details_basic");
    let cell =
        preview::cells(scene.as_ref().map_or(preview::Cells::Four, |s| s.cells)).into_iter().find(|c| c.tag == "en-96");
    match (scene, cell) {
        (Some(s), Some(c)) => preview::harness(&s, &c, &Fakes::new()).render().px.iter().any(|p| p & 0x00FF_FFFF != 0),
        _ => false,
    }
}

/// The file's bytes: Ok(None) when it does not exist, Err when it cannot be read whole or is over `cap`.
fn read_capped(path: &std::path::Path, cap: u64) -> Result<Option<Vec<u8>>, ()> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    cap_read(f, cap).map(Some)
}

/// All of `r` when it holds at most `cap` bytes.
fn cap_read(r: impl std::io::Read, cap: u64) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    r.take(cap + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > cap {
        return Err(());
    }
    Ok(bytes)
}

/// files.json is readable (at most 64 KiB) and parses; absent means the defaults. Never Store::load: that may create
/// or rewrite the file.
fn options_sound() -> bool {
    let Some(spec) = options::spec() else { return true };
    match read_capped(&spec.path, OPTIONS_CAP) {
        Ok(None) => true,
        Ok(Some(bytes)) => options::parse_file(&bytes).is_ok(),
        Err(()) => false,
    }
}

/// theme.json parses; absent means the defaults.
fn theme_sound() -> bool {
    let Some(path) = oled_config::theme::theme_path() else { return true };
    match read_capped(&path, THEME_CAP) {
        Ok(None) => true,
        Ok(Some(bytes)) => oled_config::theme::parse(&bytes).is_ok(),
        Err(()) => false,
    }
}

struct Real;

impl Parts for Real {
    fn strings(&self) -> bool {
        strings_sound(hook("drop-it").then_some(0))
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

/// --selftest: no mutex, no forward, no window, no network. A watchdog exits 1 at 9 s.
pub fn run(nonce: &str) -> i32 {
    let log = oled_sys::logger::start("", &crate::LOG_UI);
    let code = checked(nonce);
    oled_sys::logger::stop(log);
    code
}

fn checked(nonce: &str) -> i32 {
    let paused = hook("pause");
    std::thread::spawn(move || {
        if paused {
            return;
        }
        std::thread::sleep(WATCHDOG);
        oled_sys::log!("files: selftest: timed out");
        std::process::exit(1);
    });
    if let Some(bad) = first_failure(&Real) {
        oled_sys::log!("files: selftest: {} failed", bad.name());
        return 1;
    }
    #[cfg(debug_assertions)]
    if paused {
        pause();
    }
    match oled_sys::selftest::signal("files", "selftest", nonce) {
        Ok(()) => 0,
        Err(e) => {
            oled_sys::log!("files: selftest: no signal ({:?})", e.kind());
            1
        }
    }
}

/// The "pause" hook: prints "paused", then waits for FILES_TEST_PARENT to exit (at most a minute).
#[cfg(debug_assertions)]
fn pause() {
    use std::io::Write as _;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "paused");
    let _ = out.flush();
    let Some(pid) = std::env::var("FILES_TEST_PARENT").ok().and_then(|p| p.parse::<u32>().ok()) else { return };
    // SAFETY: a plain open by id for waiting on.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else { return };
    // SAFETY: a fresh handle, owned by nothing else.
    let parent = unsafe { OwnedHandle::from_raw_handle(h.0 as RawHandle) };
    // SAFETY: the handle is valid while `parent` lives.
    unsafe { WaitForSingleObject(HANDLE(parent.as_raw_handle()), 60_000) };
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
        assert_eq!(first_failure(&Fake([true, false, false, true])), Some(Check::Render));
    }

    #[test]
    fn the_real_strings_and_render_are_sound() {
        assert!(strings_sound(None), "every key has both languages");
        assert!(!strings_sound(Some(0)), "one blank Italian text is found");
        assert!(render_sound(), "details_basic is not black");
    }

    #[test]
    fn a_file_over_the_cap_is_refused() {
        assert_eq!(cap_read(&b"{}"[..], 8), Ok(b"{}".to_vec()));
        assert_eq!(cap_read(&[b' '; 8][..], 8).map(|b| b.len()), Ok(8));
        assert_eq!(cap_read(&[b' '; 9][..], 8), Err(()));
    }
}
