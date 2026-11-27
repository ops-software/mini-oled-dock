//! About: the version, Beta while app::STAGE_BETA, the licence line. A kit Dialog with one button (TS4).

use oled_trust::manifest::release::Version;
use oled_ui::widgets::DialogSpec;
use oled_ui::{CommandId, EventCx, UiLang};

use super::commands::ABOUT;
use super::screen::TasksScreen;
use crate::app::STAGE_BETA;
use crate::strings::{app_name, k, t};

/// What the dialog's one button answers: the dialog closes itself and dispatch ignores the id.
const CLOSE: CommandId = CommandId(0x4000);

/// The dialog's content: the display name as the title; the version (with "Beta" while Tasks is beta) and the licence
/// line as the body; one button that closes it.
pub fn spec(lang: UiLang, version: Version, beta: bool) -> DialogSpec {
    let mut body = oled_sys::i18n::subst(&t(lang, k::TASKS_ABOUT_VERSION), &[("v", &version.to_string())]);
    if beta {
        body.push(' ');
        body.push_str(&t(lang, k::TASKS_ABOUT_BETA));
    }
    body.push('\n');
    body.push_str(&t(lang, k::TASKS_ABOUT_LEGAL));
    DialogSpec {
        title: app_name(lang),
        body: body.into(),
        buttons: vec![(t(lang, k::TASKS_ABOUT_CLOSE), CLOSE)],
        wide: false,
    }
}

/// ABOUT (the More menu): opens the dialog; its button (or Esc) closes it.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    if id != ABOUT {
        return;
    }
    if s.notices.dialog_open() {
        return;
    }
    s.notices.open_dialog(cx, spec(cx.lang(), Version::current(), STAGE_BETA));
}
