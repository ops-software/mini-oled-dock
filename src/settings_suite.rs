//! The Suite page of Settings (dock-suite 2.6): the suite apps, their switches and the Ctrl+Shift+Esc takeover.
//!
//! Declared as a child module of `settings_pages` (`#[path]`).

use oled_trust::frame::tasks::Show;

use crate::config::Config;
use crate::glyphs;
use crate::i18n::{k, t};
use crate::settings_pages::{Action, Button, Control, Ctx, Glyph, Row, Setting};
use crate::ui::suite_links::{self, App, AppState};
use crate::ui::takeover::{self, Block, StageView, TakeoverView};
use oled_sys::i18n::{subst, S};

/// What the Suite page shows: both apps and the takeover.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SuiteView {
    pub tasks: AppState,
    pub files: AppState,
    pub takeover: Option<TakeoverView>,
}

/// A change made on the Suite page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuiteEdit {
    Component(App, bool),
    Takeover(bool),
}

/// The live view: the cached states of both apps and the takeover's availability.
pub(crate) fn view_now() -> SuiteView {
    SuiteView {
        tasks: suite_links::state(App::Tasks),
        files: suite_links::state(App::Files),
        takeover: Some(takeover::view()),
    }
}

/// The deterministic view of the goldens: both apps installed, on and in beta; the takeover off and blocked by the beta.
pub(crate) fn preview_view() -> SuiteView {
    let app = AppState { installed: true, enabled: true, stage: StageView::Beta, version: Some("1.8.0".into()) };
    SuiteView { tasks: app.clone(), files: app, takeover: Some(TakeoverView { on: false, avail: Err(Block::Beta) }) }
}

/// The takeover text key of each reason it cannot be on. Pure.
pub(crate) fn block_key(b: Block) -> S {
    match b {
        Block::TasksMissing => k::TAKEOVER_MISSING,
        Block::TasksOff => k::TAKEOVER_OFF,
        Block::Beta => k::TAKEOVER_BETA,
        Block::Build => k::TAKEOVER_BUILD,
        Block::NoFallback => k::TAKEOVER_NO_FALLBACK,
        Block::Paused => k::TAKEOVER_PAUSED,
    }
}

/// An app's usable state: installed and switched on.
fn usable(s: &AppState) -> bool {
    s.installed && s.enabled
}

/// The description under an app's switch. Pure.
fn app_desc(s: &AppState) -> String {
    if !s.installed {
        return t(k::SUITE_NOT_INSTALLED).to_string();
    }
    if !s.enabled {
        return t(k::SUITE_OFF).to_string();
    }
    let v = s.version.as_deref().unwrap_or("");
    let key = if s.stage == StageView::Stable { k::SUITE_VERSION } else { k::SUITE_BETA_VERSION };
    subst(t(key), &[("v", v)])
}

fn with_app(key: S, app: App) -> String {
    subst(t(key), &[("app", suite_links::name(app))])
}

pub(crate) fn rows(c: &Config, ctx: &Ctx<'_>) -> Vec<Row> {
    let v = ctx.suite;
    let app_row = |icon: Glyph, app: App, state: &AppState, setting: Setting| {
        let on = is_on(c, setting);
        Row {
            icon: Some(icon),
            label: suite_links::name(app).to_string(),
            desc: Some(app_desc(state)),
            control: Control::Toggle { setting, on, enabled: true },
        }
    };
    let open = |app: App, state: &AppState| Button {
        action: Action::OpenApp(app),
        text: suite_links::name(app).into(),
        enabled: usable(state),
    };
    let blocked = TakeoverView { on: false, avail: Err(Block::TasksMissing) };
    let tk = v.takeover.unwrap_or(blocked);
    let desc = match tk.avail {
        Ok(()) => with_app(k::SUITE_CSE_HELP, App::Tasks),
        Err(b) => with_app(block_key(b), App::Tasks),
    };
    vec![
        Row { icon: None, label: t(k::SUITE_APPS).to_string(), desc: None, control: Control::Header { value: None } },
        app_row(glyphs::bars_shape, App::Tasks, &v.tasks, Setting::SuiteTasks),
        app_row(glyphs::folder_shape, App::Files, &v.files, Setting::SuiteFiles),
        Row {
            icon: Some(glyphs::square_shape),
            label: t(k::SUITE_OPEN).to_string(),
            desc: None,
            control: Control::Buttons(vec![open(App::Tasks, &v.tasks), open(App::Files, &v.files)]),
        },
        Row {
            icon: None,
            label: t(k::SUITE_KEYBOARD).to_string(),
            desc: None,
            control: Control::Header { value: None },
        },
        Row {
            icon: Some(glyphs::keycap_shape),
            label: with_app(k::SUITE_CSE_LABEL, App::Tasks),
            desc: Some(desc),
            control: Control::Toggle {
                setting: Setting::TakeoverCtrlShiftEsc,
                on: is_on(c, Setting::TakeoverCtrlShiftEsc),
                enabled: takeover::toggle_enabled(tk),
            },
        },
    ]
}

pub(crate) fn is_on(c: &Config, s: Setting) -> bool {
    match s {
        Setting::SuiteTasks => c.suite_tasks,
        Setting::SuiteFiles => c.suite_files,
        Setting::TakeoverCtrlShiftEsc => c.takeover_ctrl_shift_esc,
        _ => false,
    }
}

pub(crate) fn flip(c: &mut Config, s: Setting) {
    let on = !is_on(c, s);
    match s {
        Setting::SuiteTasks => apply(c, SuiteEdit::Component(App::Tasks, on)),
        Setting::SuiteFiles => apply(c, SuiteEdit::Component(App::Files, on)),
        Setting::TakeoverCtrlShiftEsc => apply(c, SuiteEdit::Takeover(on)),
        _ => {}
    }
}

/// Switching Tasks off also turns its takeover off in the same config: a component off undoes its takeovers.
pub(crate) fn apply(c: &mut Config, e: SuiteEdit) {
    match e {
        SuiteEdit::Component(App::Tasks, on) => {
            c.suite_tasks = on;
            if !on {
                c.takeover_ctrl_shift_esc = false;
            }
        }
        SuiteEdit::Component(App::Files, on) => c.suite_files = on,
        SuiteEdit::Takeover(on) => c.takeover_ctrl_shift_esc = on,
    }
}

pub(crate) fn run(a: Action) {
    match a {
        Action::OpenApp(App::Tasks) => {
            suite_links::open_tasks(Show::default());
        }
        Action::OpenApp(App::Files) => {
            suite_links::open_files();
        }
        _ => {}
    }
}

/// A diamond outline with a centre dot, in the 16 x 16 box of the page icons.
pub(crate) fn suite_glyph(u: f32, v: f32) -> bool {
    const HALF: f32 = 0.75;
    const RING: [(f32, f32); 5] = [(8.0, 1.5), (14.5, 8.0), (8.0, 14.5), (1.5, 8.0), (8.0, 1.5)];
    let edge = RING.windows(2).any(|w| crate::glyphs::dist_seg(u, v, w[0].0, w[0].1, w[1].0, w[1].1) <= HALF);
    let (dx, dy) = (u - 8.0, v - 8.0);
    edge || dx * dx + dy * dy <= 1.6 * 1.6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_italian;
    use crate::settings_pages::{safety::SafetyView, Memory};
    use crate::start_import;
    use crate::update::{Outcome, Phase, UpdateStatus};
    use crate::update_release::Version;

    fn ctx_rows(c: &Config, v: &SuiteView) -> Vec<Row> {
        let st = UpdateStatus {
            installed: Version { major: 1, minor: 8, patch: 0 },
            phase: Phase::Idle,
            available: None,
            last_check: String::new(),
            outcome: Outcome::None,
            staged: false,
        };
        let safety = SafetyView::default();
        let cx = Ctx {
            screens: &[],
            picker_open: false,
            chooser_open: false,
            tray: &[],
            import: start_import::Status::Idle,
            update: &st,
            memory: Memory::default(),
            data_dir: None,
            log_path: None,
            trust_elevated: false,
            powertoys: false,
            chord_held: None,
            battery_note: false,
            keys_note: false,
            suite: v,
            safety: &safety,
        };
        rows(c, &cx)
    }

    const BLOCKS: [Block; 6] =
        [Block::TasksMissing, Block::TasksOff, Block::Beta, Block::Build, Block::NoFallback, Block::Paused];

    fn toggle_of(r: &Row) -> (Setting, bool, bool) {
        match r.control {
            Control::Toggle { setting, on, enabled } => (setting, on, enabled),
            ref other => panic!("not a toggle: {other:?}"),
        }
    }

    #[test]
    fn suite_rows_preview() {
        let c = Config::default();
        for italian in [false, true] {
            with_italian(italian, || {
                let r = ctx_rows(&c, &preview_view());
                assert_eq!(r.len(), 6);
                assert_eq!(r[0].label, t(k::SUITE_APPS));
                assert_eq!(r[1].label, suite_links::name(App::Tasks));
                assert_eq!(r[1].desc.as_deref(), Some(subst(t(k::SUITE_BETA_VERSION), &[("v", "1.8.0")]).as_str()));
                assert_eq!(toggle_of(&r[1]), (Setting::SuiteTasks, true, true));
                assert_eq!(r[2].label, suite_links::name(App::Files));
                assert_eq!(toggle_of(&r[2]), (Setting::SuiteFiles, true, true));
                assert_eq!(r[3].label, t(k::SUITE_OPEN));
                let Control::Buttons(b) = &r[3].control else { panic!("buttons") };
                assert_eq!(b.len(), 2);
                assert_eq!(b[0].action, Action::OpenApp(App::Tasks));
                assert_eq!(b[1].action, Action::OpenApp(App::Files));
                assert_eq!(r[4].label, t(k::SUITE_KEYBOARD));
                assert_eq!(r[5].label, subst(t(k::SUITE_CSE_LABEL), &[("app", suite_links::name(App::Tasks))]));
                assert_eq!(r[5].desc, Some(with_app(k::TAKEOVER_BETA, App::Tasks)));
                assert_eq!(toggle_of(&r[5]), (Setting::TakeoverCtrlShiftEsc, false, false));
                assert!(r.iter().all(|r| r.desc.as_deref() != Some("")));
            });
        }
        let r =
            ctx_rows(&c, &SuiteView { takeover: Some(TakeoverView { on: false, avail: Ok(()) }), ..preview_view() });
        assert_eq!(r[5].desc, Some(with_app(k::SUITE_CSE_HELP, App::Tasks)));
        assert!(toggle_of(&r[5]).2);
    }

    #[test]
    fn switching_tasks_off_turns_the_takeover_off() {
        let mut c = Config { takeover_ctrl_shift_esc: true, ..Config::default() };
        apply(&mut c, SuiteEdit::Component(App::Files, false));
        assert!(!c.suite_files && c.suite_tasks && c.takeover_ctrl_shift_esc, "Folio has no takeover");
        flip(&mut c, Setting::SuiteTasks);
        assert!(!c.suite_tasks && !c.takeover_ctrl_shift_esc);
        flip(&mut c, Setting::SuiteTasks);
        assert!(c.suite_tasks && !c.takeover_ctrl_shift_esc, "switching on does not bring the takeover back");
        flip(&mut c, Setting::TakeoverCtrlShiftEsc);
        assert!(c.takeover_ctrl_shift_esc && is_on(&c, Setting::TakeoverCtrlShiftEsc));
        assert!(!is_on(&c, Setting::UpdateCheck), "other settings are not ours");
    }

    #[test]
    fn takeover_toggle_follows_toggle_enabled() {
        for b in BLOCKS {
            let mut c = Config::default();
            let v = |on| SuiteView { takeover: Some(TakeoverView { on, avail: Err(b) }), ..preview_view() };
            assert!(!toggle_of(&ctx_rows(&c, &v(false))[5]).2, "{b:?}: blocked and off");
            c.takeover_ctrl_shift_esc = true;
            let r = ctx_rows(&c, &v(true));
            assert_eq!(toggle_of(&r[5]), (Setting::TakeoverCtrlShiftEsc, true, true), "{b:?}: blocked and on");
        }
    }

    #[test]
    fn every_block_has_its_text() {
        let keys: Vec<S> = BLOCKS.iter().map(|b| block_key(*b)).collect();
        for (i, a) in keys.iter().enumerate() {
            assert!(keys[i + 1..].iter().all(|b| b != a), "keys are distinct");
        }
        for italian in [false, true] {
            with_italian(italian, || {
                for key in &keys {
                    assert!(!t(*key).is_empty());
                }
            });
        }
        assert_eq!(block_key(Block::Beta), k::TAKEOVER_BETA);
        assert_eq!(block_key(Block::NoFallback), k::TAKEOVER_NO_FALLBACK);
    }

    #[test]
    fn not_installed_desc() {
        let missing = AppState::default();
        assert_eq!(app_desc(&missing), t(k::SUITE_NOT_INSTALLED));
        let off = AppState { installed: true, enabled: false, ..AppState::default() };
        assert_eq!(app_desc(&off), t(k::SUITE_OFF));
        let stable =
            AppState { installed: true, enabled: true, stage: StageView::Stable, version: Some("2.0.1".into()) };
        assert_eq!(app_desc(&stable), subst(t(k::SUITE_VERSION), &[("v", "2.0.1")]));
        let v = SuiteView { tasks: missing, ..preview_view() };
        let r = ctx_rows(&Config::default(), &v);
        assert_eq!(r[1].desc.as_deref(), Some(t(k::SUITE_NOT_INSTALLED)));
    }

    #[test]
    fn open_buttons_follow_usable() {
        let off = AppState { installed: true, enabled: false, ..AppState::default() };
        let v = SuiteView { tasks: AppState::default(), files: off, ..preview_view() };
        let r = ctx_rows(&Config::default(), &v);
        let Control::Buttons(b) = &r[3].control else { panic!("buttons") };
        assert!(!b[0].enabled && !b[1].enabled, "missing and off are not openable");
        let r = ctx_rows(&Config::default(), &preview_view());
        let Control::Buttons(b) = &r[3].control else { panic!("buttons") };
        assert!(b[0].enabled && b[1].enabled);
    }

    #[test]
    fn the_live_view_is_the_cached_state() {
        let v = view_now();
        assert_eq!(v.tasks, suite_links::state(App::Tasks));
        assert!(v.takeover.is_some());
    }
}
