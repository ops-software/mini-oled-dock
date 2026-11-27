//! The context menu of a row or a selection (T3.9): the verb with its description and chord, End now, End process
//! tree, Open Windows Task Manager; refused items stay in the menu, disabled, with their reason. Nothing asks for a
//! confirmation: the menu is the protection (SPEC-tasks 5.4.5).

use oled_sysmon::ProcKey;
use oled_ui::model::{ListModel, RowKey};
use oled_ui::node::Text;
use oled_ui::verbs::Verb;
use oled_ui::{Anchor, CommandId, EventCx, MenuItem, PointP, UiLang};

use super::actions::{self, RowVerb};
use super::commands::{CLOSE_OR_END, END_NOW, END_TREE, OPEN_WINDOWS_TM};
use super::list::SEP;
use super::order;
use super::rows::{RowEntry, RowKind};
use super::screen::TasksScreen;
use crate::app::access::{self, Refusal};
use crate::app::exec::KillGuard;
use crate::model::view::ViewMode;
use crate::strings::{k, t};

/// One row the menu acts on, as the menu shows it.
#[derive(Clone, Debug)]
struct MenuRow {
    key: RowKey,
    kind: RowKind,
    verb: RowVerb,
    /// The app's name (a group) or the image name (a process).
    name: String,
    members: usize,
    /// Every member refused: the reason of the first (None for a process that is gone).
    refused: Option<Option<Text>>,
}

/// The refusal of a row: Some when every member is refused (the item is disabled), with the reason of the first.
fn refusal_of(s: &mut TasksScreen, members: &[ProcKey], g: &KillGuard, lang: UiLang) -> Option<Option<Text>> {
    let mut first: Option<(Refusal, ProcKey)> = None;
    for m in members {
        let probe = s.p.actions.probe(*m);
        let f = access::facts(*m, s.v.procs(), &s.v.frame().paths, &s.deps.system_root, probe);
        match access::menu_refusal(&f, g) {
            Some(r) => {
                first.get_or_insert((r, *m));
            }
            None => return None,
        }
    }
    let (r, key) = first?;
    Some(access::why(r, key, g).map(|why| t(lang, why)))
}

/// The row's name for the menu: a group's label without its count, a process's image name.
fn name_of(s: &TasksScreen, r: &RowEntry, label: &str, members: usize) -> String {
    match r.kind {
        RowKind::Proc(p) => {
            let mut n = String::new();
            s.v.proc_name(p, &mut n);
            n
        }
        _ => label.strip_suffix(&format!(" ({members})")).unwrap_or(label).to_string(),
    }
}

/// A process row's title: its image name and its PID.
fn proc_title(name: &str, pid: u32) -> String {
    let mut s = String::with_capacity(name.len() + 12);
    s.push_str(name);
    s.push_str(SEP);
    s.push_str(&pid.to_string());
    s
}

fn chord(v: Verb, lang: UiLang) -> Option<Text> {
    v.chord().map(|c| Text::from(c.label(lang)))
}

fn item(id: CommandId, label: Text, chord: Option<Text>, enabled: bool, note: Option<Text>) -> MenuItem {
    MenuItem::Custom { id, label, glyph: None, chord, enabled, checked: None, note, danger: false }
}

/// The menu of `rows` (one row, or a multi-selection) in `mode`.
fn items(rows: &[MenuRow], mode: ViewMode, lang: UiLang) -> Vec<MenuItem> {
    let subst = |key, args: &[(&str, &str)]| Text::from(oled_sys::i18n::subst(&t(lang, key), args));
    let mut v = Vec::new();
    match rows {
        [] => return v,
        [r] => {
            let n = r.members.to_string();
            let title = match r.kind {
                RowKind::Proc(p) => Text::from(proc_title(&r.name, p.pid)),
                _ => subst(k::TASKS_MENU_TITLE_GROUP, &[("app", &r.name), ("n", &n)]),
            };
            v.push(MenuItem::Header(title));
            let enabled = r.refused.is_none();
            let reason = r.refused.clone().flatten();
            let (label, hint, desc) = match r.verb {
                RowVerb::Close => (
                    subst(k::TASKS_VERB_CLOSE_APP, &[("app", &r.name)]),
                    chord(Verb::Close, lang),
                    Verb::Close.description(lang),
                ),
                RowVerb::End => (
                    subst(k::TASKS_VERB_END_APP, &[("app", &r.name)]),
                    chord(Verb::End, lang),
                    Verb::End.description(lang),
                ),
                RowVerb::EndTask => (t(lang, k::TASKS_VERB_END_TASK), None, t(lang, k::TASKS_VERB_END_TASK_DESC)),
            };
            v.push(item(CLOSE_OR_END, label, hint, enabled, if enabled { Some(desc) } else { reason.clone() }));
            // Part of Windows: End task is the one way, and the keys do not reach it.
            if r.verb != RowVerb::EndTask {
                v.push(item(END_NOW, Verb::EndNow.label(lang), chord(Verb::EndNow, lang), enabled, reason.clone()));
            }
            if matches!(r.kind, RowKind::Proc(_)) && matches!(mode, ViewMode::Tree | ViewMode::List) {
                v.push(item(END_TREE, t(lang, k::TASKS_VERB_END_TREE), None, enabled, reason));
            }
        }
        many => {
            let n = many.len().to_string();
            let title = subst(k::TASKS_MENU_TITLE_MANY, &[("n", &n)]);
            v.push(MenuItem::Header(title));
            // Refused rows are skipped by the executor and named in their notes; all refused: disabled.
            let enabled = many.iter().any(|r| r.refused.is_none());
            let reason = if enabled { None } else { many.iter().find_map(|r| r.refused.clone().flatten()) };
            if many.iter().all(|r| r.verb == RowVerb::Close) {
                let label = subst(k::TASKS_VERB_CLOSE_MANY, &[("n", &n)]);
                v.push(item(CLOSE_OR_END, label, chord(Verb::Close, lang), enabled, reason.clone()));
            }
            let keys = !many.iter().any(|r| r.verb == RowVerb::EndTask);
            let hint = if keys { chord(Verb::EndNow, lang) } else { None };
            v.push(item(END_NOW, subst(k::TASKS_VERB_END_MANY_NOW, &[("n", &n)]), hint, enabled, reason));
        }
    }
    v.push(MenuItem::Separator);
    let tm = t(lang, k::TASKS_MORE_WINDOWS_TM);
    v.push(item(OPEN_WINDOWS_TM, tm, None, true, Some(t(lang, k::TASKS_MORE_WINDOWS_TM_DESC))));
    v
}

/// TableOut::ContextMenu (right click, Shift+F10, the Menu key), taken at the end of the call with the whole screen
/// (actions::end_call). The kit has selected the row unless the selection held it: the menu acts on the selection
/// then, else on the row; a row with no verb (a section, the hardware row, a ghost) opens no menu. While it is open
/// the order holds, and a pick acts on the rows it showed.
pub fn open(cx: &mut EventCx, s: &mut TasksScreen, key: Option<RowKey>, at: PointP) {
    let Some(key) = key else { return };
    let lang = cx.lang();
    let mut keys = actions::selected_keys(&s.table);
    if !keys.contains(&key) {
        keys = vec![key];
    }
    let guard = actions::guard_of(&s.p.actions, &s.v, s.deps.install_dir.as_deref());
    let mut rows: Vec<MenuRow> = Vec::new();
    for key in keys {
        let model = s.table.model();
        let Some(i) = model.row_of(key) else { continue };
        let Some(r) = model.rows.get(i).cloned() else { continue };
        let label = model.name(i).to_string();
        let Some(verb) = actions::row_verb(&s.p.actions, &s.v, &r) else { continue };
        let members = actions::members_of(&s.p.actions, &r);
        if members.is_empty() {
            continue;
        }
        let refused = refusal_of(s, &members, &guard, lang);
        let name = name_of(s, &r, &label, members.len());
        rows.push(MenuRow { key, kind: r.kind, verb, name, members: members.len(), refused });
    }
    if rows.is_empty() {
        return;
    }
    cx.open_menu(items(&rows, s.v.mode(), lang), Anchor::Point(at));
    order::note_menu_open(&mut s.p.order);
    s.p.actions.set_menu(rows.iter().map(|r| r.key).collect());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::args::PreviewLang;
    use crate::app::exec::RecordingExecutor;
    use crate::app::launch::RecordingLauncher;
    use crate::ui::grouped::HostEnv;
    use crate::ui::scene::{self, Scene, SceneProc, SceneWindow};
    use oled_ui::harness::Harness;
    use oled_ui::PointL;
    use std::sync::Arc;

    fn proc(pid: u32, image: &str, path: &str) -> SceneProc {
        SceneProc {
            pid,
            created: 133_500_000_000_000_000 + u64::from(pid) * 1000,
            parent: 1000,
            session: 1,
            image: image.to_string(),
            path: Some(path.to_string()),
            cpu: vec![0.2],
            ws_private: 20 << 20,
            io: vec![0.0],
            threads: 4,
            handles: 100,
        }
    }

    /// The T3c scene with the dock beside tasks.exe and Tasks itself, each with a window; Tasks is pid 6500, Quill
    /// can only be ended by an administrator.
    fn rig() -> Harness<TasksScreen> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t3c_actions.json");
        let mut s = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
        s.size = [760, 720];
        s.procs.push(proc(6000, "dock.exe", "C:\\Apps\\Suite\\dock.exe"));
        s.procs.push(proc(6500, "tasks.exe", "C:\\Apps\\Suite\\tasks.exe"));
        s.windows.push(SceneWindow { pid: 6000, identity: "dock-id".into(), hung: false });
        s.windows.push(SceneWindow { pid: 6500, identity: "tasks-id".into(), hung: false });
        let exec = Arc::new(RecordingExecutor::default());
        let mut h = scene::open(&s, PreviewLang::En, 96, Arc::new(RecordingLauncher::default()), exec)
            .unwrap_or_else(|e| panic!("{e}"));
        let quill = ProcKey { pid: 5000, created: 133_500_000_005_000_000 };
        let sc = h.screen_mut();
        sc.p.actions.set_env(HostEnv { shell_pid: Some(1000), own_pid: 6500, session: 1 });
        sc.p.actions.set_fixture_limited(vec![quill]);
        sc.deps.install_dir = Some(std::path::PathBuf::from("C:\\Apps\\Suite"));
        h
    }

    /// Right-clicks the row of `pid` (a process row, or the group whose oldest member it is) and returns the menu.
    fn menu_of(h: &mut Harness<TasksScreen>, pid: u32) -> Vec<MenuItem> {
        let row = {
            let g = h.screen().parts().grouped.grouping();
            let grp = g.groups.iter().find(|x| x.members.first().is_some_and(|m| m.pid == pid)).expect("a group");
            let key = match grp.key {
                crate::model::group::AppKey::Single(p) => crate::ui::rows::proc_key(p),
                other => crate::ui::rows::group_key(other),
            };
            h.screen().table().model().row_of(key).expect("a row")
        };
        let r = h.screen().table().row_rect(row).expect("on screen");
        h.right_click(PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 });
        h.menus().last().expect("a menu").items.clone()
    }

    /// The verb item: (enabled, note).
    fn verb_item(items: &[MenuItem]) -> (bool, Option<String>) {
        items
            .iter()
            .find_map(|i| match i {
                MenuItem::Custom { id, enabled, note, .. } if *id == CLOSE_OR_END => {
                    Some((*enabled, note.as_ref().map(|n| n.to_string())))
                }
                _ => None,
            })
            .expect("the verb item")
    }

    #[test]
    fn menu_disabled_reasons() {
        let mut h = rig();
        let cases = [
            (600, "Windows stops if this ends"),
            (800, "Windows services stop from Services"),
            (6000, "Use Exit dock"),
            (6500, "Use Close (Ctrl+W)"),
            (1000, "The taskbar and desktop would close"),
            (5000, "Needs administrator rights. Use Windows Task Manager for this one."),
        ];
        for (pid, why) in cases {
            let items = menu_of(&mut h, pid);
            assert_eq!(verb_item(&items), (false, Some(why.to_string())), "pid {pid}");
            // Every act of the row is disabled with the reason; Open Windows Task Manager stays.
            for i in &items {
                if let MenuItem::Custom { id, enabled, .. } = i {
                    assert_eq!(*enabled, *id == OPEN_WINDOWS_TM, "pid {pid}: {id:?}");
                }
            }
            h.key("Esc");
        }
        // An ordinary app: enabled, with the verb's description.
        let items = menu_of(&mut h, 2000);
        assert_eq!(verb_item(&items), (true, Some("Asks it to close. It may ask to save.".to_string())));
    }
}
