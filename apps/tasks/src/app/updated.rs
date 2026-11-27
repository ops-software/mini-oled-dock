//! After an update: the registered "mini-oled-dock.Updated" message with its notice and the restart (TS3), and the
//! installed it.lang (TS2).

use std::path::Path;
use std::sync::OnceLock;

use oled_sys::lang::{LangFile, LANG_MAX_BYTES};
use oled_trust::manifest::release::Version;
use oled_ui::{CommandId, EventCx, Reply};
use windows::core::{w, HSTRING};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;

use crate::strings::{app_name, k, t};
use crate::ui::commands::RESTART_TO_UPDATE;
use crate::ui::notice::Notices;
use crate::ui::screen::TasksScreen;

/// The version a scene's "updated" line pretends to find on disk (scenes and Harness tests only).
const FIXTURE_NEWER: Version = Version { major: 9, minor: 9, patch: 9 };

/// True when the folder is an installed image: dock.exe and suite.json sit beside tasks.exe.
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

/// <install dir>\lang\it.lang for the installed image with the suite version; any failure leaves the compiled
/// table and writes one log line.
pub fn load_lang_file(install_dir: Option<&Path>, italian: bool) {
    let Some(dir) = install_dir else { return };
    let read = || oled_sys::datadir::read_file(&dir.join("lang").join("it.lang"), LANG_MAX_BYTES as u64, true);
    match lang_file_rules(italian, is_installed(dir), read, Version::current()) {
        Ok(Some(f)) => {
            oled_sys::i18n::set_lang_file(Box::leak(Box::new(f)));
        }
        Ok(None) => {}
        Err(why) => oled_sys::log!("tasks: lang\\it.lang not used ({why})"),
    }
}

#[derive(Default)]
pub struct UpdState {
    /// Scenes and Harness tests: the disk is read as holding a newer version.
    fixture_newer: bool,
}

impl UpdState {
    /// For scenes and Harness tests: the next Updated message finds a newer file.
    pub fn set_fixture_newer(&mut self) {
        self.fixture_newer = true;
    }
}

/// The id Windows gives the registered message (the one app::main routes to the screen).
pub fn updated_message() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| {
        // SAFETY: a NUL-terminated string that lives for the call.
        unsafe { RegisterWindowMessageW(&HSTRING::from(oled_sys::selftest::UPDATED_MESSAGE)) }
    })
}

/// True only when the file on disk is newer than the running version: the message proves nothing about its sender,
/// so the notice rests on the file alone.
pub fn needs_notice(disk: Option<Version>, current: Version) -> bool {
    disk.is_some_and(|d| d > current)
}

/// The file version of the exe on disk (the fixed file info's major.minor.patch); None when it cannot be read.
pub fn disk_version(exe: &Path) -> Option<Version> {
    let name = HSTRING::from(exe.as_os_str());
    // SAFETY: name is NUL-terminated and lives for the call.
    let size = unsafe { GetFileVersionInfoSizeW(&name, None) };
    if size == 0 || size > 1 << 20 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buf is writable for size bytes, which is what is passed.
    unsafe { GetFileVersionInfoW(&name, None, size, buf.as_mut_ptr().cast()) }.ok()?;
    let mut at: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: buf holds the block just read; at and len are writable.
    let found = unsafe { VerQueryValueW(buf.as_ptr().cast(), w!("\\"), &mut at, &mut len) };
    if !found.as_bool() || at.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: VerQueryValueW pointed inside buf at a VS_FIXEDFILEINFO of at least that many bytes (checked above); it
    // may be unaligned, so it is read by value.
    let info = unsafe { at.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    if info.dwSignature != 0xFEEF_04BD {
        return None;
    }
    Some(Version {
        major: info.dwFileVersionMS >> 16,
        minor: info.dwFileVersionMS & 0xFFFF,
        patch: info.dwFileVersionLS >> 16,
    })
}

/// A routed registered message (Screen::app_message): only the Updated one counts, and only a newer file on disk shows
/// the notice (kept until Esc).
pub fn on_message(cx: &mut EventCx, st: &mut UpdState, n: &mut Notices, msg: u32, wparam: usize) -> Reply {
    let _ = wparam;
    if msg == 0 || msg != updated_message() {
        return Reply::None;
    }
    let disk = if st.fixture_newer {
        Some(FIXTURE_NEWER)
    } else {
        std::env::current_exe().ok().and_then(|p| disk_version(&p))
    };
    if !needs_notice(disk, Version::current()) {
        oled_sys::log!("tasks: updated message, the file on disk is not newer");
        return Reply::None;
    }
    let lang = cx.lang();
    let text = oled_sys::i18n::subst(&t(lang, k::TASKS_UPDATED_TEXT), &[("name", app_name(lang).as_ref())]);
    n.show_kept(cx, text.into(), Some((t(lang, k::TASKS_UPDATED_ACTION), RESTART_TO_UPDATE)));
    oled_sys::log!("tasks: update notice shown");
    Reply::None
}

/// The restart in its order: the single-instance guard goes first (the new process takes the name), then the new
/// process starts. False when the start failed.
pub fn restart_in_order(release: impl FnOnce(), start: impl FnOnce() -> Result<(), u32>) -> bool {
    release();
    match start() {
        Ok(()) => true,
        Err(code) => {
            oled_sys::log!("tasks: restart failed ({code:#x})");
            false
        }
    }
}

/// RESTART_TO_UPDATE: drop the mutex guard, start the new process through the launcher, close this window.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    if id != RESTART_TO_UPDATE {
        return;
    }
    let launcher = std::sync::Arc::clone(&s.deps.launcher);
    let instance = &mut s.deps.instance;
    if restart_in_order(
        || {
            // The guard's drop releases the mutex before the new process is started.
            let _guard = instance.take();
        },
        || launcher.restart_self(),
    ) {
        cx.close_window();
    } else {
        // The mutex is gone and the window stays: say so (re-taking the guard waits for the real Guard of the instance lane).
        let lang = cx.lang();
        s.notices.show(cx, t(lang, k::TASKS_UPDATED_FAILED), None);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use oled_ui::harness::Harness;
    use oled_ui::{SizeL, Tokens, UiLang};

    use super::*;
    use crate::app::exec::RecordingExecutor;
    use crate::app::instance::Guard;
    use crate::app::launch::{LaunchReq, RecordingLauncher};
    use crate::app::Deps;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    #[test]
    fn lang_file_rules() {
        let cur = v(1, 8, 0);
        let enc = |tag: &str, ver: (u16, u16, u16)| oled_sys::lang::encode(tag, ver, &[(1, "a")]).unwrap();
        let ok = || Ok(enc("it", (1, 8, 0)));
        let boom = || -> std::io::Result<Vec<u8>> { panic!("must not read") };
        assert!(super::lang_file_rules(true, true, ok, cur).unwrap().is_some());
        assert!(super::lang_file_rules(false, true, boom, cur).unwrap().is_none(), "not Italian: not read");
        assert!(super::lang_file_rules(true, false, boom, cur).unwrap().is_none(), "not installed: not read");
        assert_eq!(super::lang_file_rules(true, true, || Ok(enc("en", (1, 8, 0))), cur).err().as_deref(), Some("tag"));
        let newer = || Ok(enc("it", (1, 8, 1)));
        assert_eq!(super::lang_file_rules(true, true, newer, cur).err().as_deref(), Some("version"));
        assert!(super::lang_file_rules(true, true, || Ok(vec![0; 8]), cur).is_err(), "garbage does not parse");
        let missing = || Err(std::io::ErrorKind::NotFound.into());
        assert!(super::lang_file_rules(true, true, missing, cur).unwrap().is_none(), "no file: silent");
    }

    #[test]
    fn updated_needs_a_newer_file() {
        let cur = v(1, 6, 0);
        assert!(needs_notice(Some(v(1, 6, 1)), cur));
        assert!(needs_notice(Some(v(2, 0, 0)), cur));
        assert!(!needs_notice(Some(cur), cur), "the same file is no update");
        assert!(!needs_notice(Some(v(1, 5, 9)), cur), "an older file is no update");
        assert!(!needs_notice(None, cur), "an unreadable file is no update");
        // The test binary has no version resource: reading it gives nothing, never a notice.
        let exe = std::env::current_exe().unwrap();
        assert!(!needs_notice(disk_version(&exe), cur));
    }

    #[test]
    fn the_registered_message_is_stable() {
        assert_ne!(updated_message(), 0);
        assert_eq!(updated_message(), updated_message());
    }

    #[test]
    fn restart_sequence_releases_first() {
        let log = Mutex::new(Vec::new());
        let ok = restart_in_order(
            || log.lock().unwrap().push("release"),
            || {
                log.lock().unwrap().push("start");
                Ok(())
            },
        );
        assert!(ok);
        assert_eq!(*log.lock().unwrap(), ["release", "start"]);
        let log = Mutex::new(Vec::new());
        let ok = restart_in_order(
            || log.lock().unwrap().push("release"),
            || {
                log.lock().unwrap().push("start");
                Err(5)
            },
        );
        assert!(!ok, "a failed start keeps the window");
        assert_eq!(*log.lock().unwrap(), ["release", "start"]);
    }

    struct FailingLauncher;

    impl crate::app::launch::Launcher for FailingLauncher {
        fn open_taskmgr(&self) -> Result<(), u32> {
            Ok(())
        }
        fn open_launch(&self, _: &str) -> Result<(), u32> {
            Ok(())
        }
        fn restart_self(&self) -> Result<(), u32> {
            Err(5)
        }
    }

    /// A failed restart keeps the window and says so (the notice is replaced, not left silent).
    #[test]
    fn failed_restart_keeps_the_window_with_a_notice() {
        let mut deps = Deps::inert(UiLang::EN, Arc::new(FailingLauncher), Arc::new(RecordingExecutor::default()));
        deps.instance = Some(Guard);
        let mut h = Harness::new(TasksScreen::new(deps), SizeL { w: 640, h: 560 }, 96, Tokens::oled());
        h.screen_mut().p.updated.set_fixture_newer();
        h.app_message(updated_message(), 0, 0);
        let before = h.render().px.clone();
        h.send(Box::new(crate::ui::screen::InjectCommand(RESTART_TO_UPDATE)));
        assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"), "the window stays: {:?}", h.last_outputs());
        assert!(h.screen().notices.line_shown(), "a notice is up");
        assert_ne!(h.render().px, before, "the notice text changed to the failure line");
    }

    /// The command: the guard is gone, exactly one restart request is recorded, nothing is started and the window
    /// closes.
    #[test]
    fn restart_releases_mutex_first() {
        let launcher = Arc::new(RecordingLauncher::default());
        let mut deps = Deps::inert(UiLang::EN, launcher.clone(), Arc::new(RecordingExecutor::default()));
        deps.instance = Some(Guard);
        let mut h = Harness::new(TasksScreen::new(deps), SizeL { w: 640, h: 560 }, 96, Tokens::oled());
        h.screen_mut().p.updated.set_fixture_newer();
        h.app_message(updated_message(), 0, 0);
        assert!(h.screen().notices.line_shown(), "the notice is up");
        assert!(h.screen().deps().instance.is_some());
        h.send(Box::new(crate::ui::screen::InjectCommand(RESTART_TO_UPDATE)));
        assert!(h.screen().deps().instance.is_none(), "the guard was dropped");
        assert_eq!(*launcher.reqs.lock().unwrap(), vec![LaunchReq::RestartSelf]);
        assert!(h.last_outputs().iter().any(|o| o == "CloseWindow"), "the window closes: {:?}", h.last_outputs());
    }
}
