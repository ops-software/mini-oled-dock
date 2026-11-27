//! TA-SUITE (TS3, TS4): the Updated notice, the restart and About, through the kit Harness. No window, no process.

use std::path::Path;
use std::sync::Arc;

use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::{LaunchReq, RecordingLauncher};
use tasks::ui::scene::{self, Scene};

fn load(name: &str) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join(format!("{name}.json"));
    Scene::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

fn rig(
    name: &str,
    lang: PreviewLang,
) -> (oled_ui::harness::Harness<tasks::ui::screen::TasksScreen>, Arc<RecordingLauncher>) {
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let h = scene::open(&load(name), lang, 96, launcher.clone(), exec).unwrap_or_else(|e| panic!("{e}"));
    (h, launcher)
}

#[test]
fn the_notice_is_kept_until_esc() {
    let (mut h, launcher) = rig("suite_notice", PreviewLang::En);
    assert!(h.screen().notices_shown(), "the notice line is up");
    assert!(h.a11y_dump().contains("was updated"), "{}", h.a11y_dump());
    assert!(h.a11y_dump().contains("Restart"));
    // Not hidden after the kit's 6 s.
    h.advance(7000);
    assert!(h.screen().notices_shown(), "no 6 s timer: it stays until Esc");
    h.key("Esc");
    assert!(!h.screen().notices_shown(), "Esc takes it away");
    assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"), "that Esc did not close the window");
    assert!(launcher.reqs.lock().unwrap().is_empty(), "nothing was started");
    h.assert_named();
}

#[test]
fn the_notice_is_in_italian() {
    let (h, _) = rig("suite_notice", PreviewLang::It);
    let dump = h.a11y_dump();
    assert!(dump.contains("Riavvia"), "{dump}");
}

#[test]
fn no_newer_file_no_notice() {
    // The Updated message with the real disk check: the test binary carries no version resource.
    let launcher = Arc::new(RecordingLauncher::default());
    let exec = Arc::new(RecordingExecutor::default());
    let mut s = load("suite_notice");
    s.input.clear();
    let mut h = scene::open(&s, PreviewLang::En, 96, launcher, exec).unwrap_or_else(|e| panic!("{e}"));
    h.app_message(tasks::app::updated::updated_message(), 0x0001_0600, 0);
    assert!(!h.screen().notices_shown());
    // A message that is not the Updated one is ignored.
    h.app_message(0xC123, 0, 0);
    assert!(!h.screen().notices_shown());
    h.assert_named();
}

#[test]
fn restart_asks_the_launcher_once_and_closes() {
    let (mut h, launcher) = rig("suite_notice", PreviewLang::En);
    h.send(Box::new(tasks::ui::screen::InjectCommand(tasks::ui::commands::RESTART_TO_UPDATE)));
    assert_eq!(*launcher.reqs.lock().unwrap(), vec![LaunchReq::RestartSelf]);
    assert!(h.last_outputs().iter().any(|o| o == "CloseWindow"));
}

#[test]
fn about_shows_the_version_and_closes() {
    let (mut h, _) = rig("suite_about", PreviewLang::En);
    let dump = h.a11y_dump();
    assert!(dump.contains("Dialog \"Tasks\""), "{dump}");
    assert!(dump.contains("Button \"Close\""), "{dump}");
    // The body (the dump does not print descriptions): version, Beta and the licence line.
    let v = oled_trust::manifest::release::Version::current();
    let body = tasks::ui::about::spec(oled_ui::UiLang::EN, v, true).body.to_string();
    assert!(body.contains(&format!("Version {v}")), "{body}");
    assert!(body.contains("Beta") && body.contains("MIT licence"), "{body}");
    assert!(!tasks::ui::about::spec(oled_ui::UiLang::EN, v, false).body.contains("Beta"), "stable has no Beta");
    // Esc closes the dialog and not the window.
    h.key("Esc");
    assert!(!h.a11y_dump().contains("Dialog"), "the dialog is gone");
    assert!(!h.last_outputs().iter().any(|o| o == "CloseWindow"));
    // Enter takes the primary button.
    let (mut h, _) = rig("suite_about", PreviewLang::En);
    h.key("Enter");
    assert!(!h.a11y_dump().contains("Dialog"));
    h.assert_named();
}

#[test]
fn about_keeps_input_from_the_window_behind() {
    let (mut h, _) = rig("suite_about", PreviewLang::En);
    let before = h.screen().view().sort().column;
    h.key("Ctrl+Shift+3");
    assert_eq!(h.screen().view().sort().column, before, "a chord of the window does nothing under the dialog");
}

#[test]
fn aumid_constant_matches() {
    assert_eq!(oled_trust::frame::tasks::AUMID, "MiniOledDock.Tasks");
}
