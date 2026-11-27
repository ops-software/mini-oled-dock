//! START quick-links popup (right click on the Start button): the entries of the native Win+X menu
//! that make sense for the dock, drawn like the app menu (`popup_app`): same rows, hover plate and
//! separators. A click closes the popup and THEN runs the action (`after_close`).
//!
//! Every launch goes through `ShellRequest::Launch`, i.e. the desktop shell as the normal user (never
//! elevated). The "(admin)" entry goes through `ShellRequest::LaunchAdmin`: Explorer runs a fixed
//! tool with the `runas` verb and Windows shows the UAC prompt (the dock's own token is never used).
//! "Search" and "Run" are a synthetic Win+S / Win+R tagged with `INJECT_TAG`; "Sign out" is the
//! `power::PowerAction::SignOut` action. The entries are static strings: no state, timer or allocation
//! survives the popup.
//!
//! Declared as a child module of `popup` (`#[path]`).

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY;

use crate::i18n::{k, t};
use crate::popup::{center_y, hline, lift, row_plate, Content, HitMap, PopupKind, Reply};
use crate::power::{self, PowerAction};
use crate::shell_thread::{self, AdminTool, ShellRequest};
use crate::surface::{rect_wh, Font, Surface, TEXT_ALPHA, WHITE};
use crate::ui::suite_links::{self, App};
use oled_sys::i18n::S;
use oled_trust::frame::tasks::Show;

/// What a row does.
#[derive(Clone, Copy)]
enum Act {
    /// Launch a target through the desktop shell (de-elevated): `ms-settings:` URI, `.msc`, exe name.
    Launch(&'static str),
    /// Fixed tool started by Explorer with the `runas` verb (UAC prompt).
    Admin(AdminTool),
    /// Win + letter (`INJECT_TAG`).
    WinKey(VIRTUAL_KEY),
    /// Sign out (`power::run(PowerAction::SignOut)`).
    SignOut,
    /// Open the dock's own Settings window.
    DockSettings,
    /// Open a suite app (Tasks or Folio).
    App(App),
}

const VK_S: VIRTUAL_KEY = VIRTUAL_KEY(0x53);
const VK_R: VIRTUAL_KEY = VIRTUAL_KEY(0x52);

/// What a row says: a key of the string table, or a suite app's display name (with the Beta tag while it is in beta).
#[derive(Clone, Copy)]
enum Label {
    Key(S),
    Name(App),
}

/// One row: action and label.
type Row = (Act, Label);

/// Rows in the order of the native Win+X menu (`None` = separator). The row id is its index + 1.
const ROWS: [Option<(Act, S)>; 19] = [
    Some((Act::Launch("ms-settings:appsfeatures"), k::START_INSTALLED_APPS)),
    Some((Act::Launch("ms-settings:powersleep"), k::START_POWER_OPTIONS)),
    Some((Act::Launch("eventvwr.msc"), k::START_EVENT_VIEWER)),
    Some((Act::Launch("ms-settings:about"), k::SETTINGS_PAGES_SYSTEM)),
    Some((Act::Launch("devmgmt.msc"), k::START_DEVICE_MANAGER)),
    Some((Act::Launch("ms-settings:network"), k::START_NETWORK_CONNECTIONS)),
    Some((Act::Launch("diskmgmt.msc"), k::START_DISK_MANAGEMENT)),
    Some((Act::Launch("compmgmt.msc"), k::START_COMPUTER_MANAGEMENT)),
    Some((Act::Launch(r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"), k::START_TERMINAL)),
    Some((Act::Admin(AdminTool::Terminal), k::START_TERMINAL_ADMIN)),
    None,
    Some((Act::Launch("taskmgr.exe"), k::DOCK_TASK_MANAGER)),
    Some((Act::Launch("ms-settings:"), k::COMMON_SETTINGS)),
    Some((Act::DockSettings, k::COMMON_DOCK_SETTINGS)),
    Some((Act::Launch("explorer.exe"), k::LAUNCHER_FILE_EXPLORER)),
    Some((Act::WinKey(VK_S), k::START_SEARCH)),
    Some((Act::WinKey(VK_R), k::LAUNCHER_STRINGS_RUN)),
    None,
    Some((Act::SignOut, k::START_SIGN_OUT)),
];

/// The rows with the suite apps that are usable: Tasks right after Task Manager, Folio right after File Explorer
/// (`None` = separator). Pure.
fn row_list(tasks: bool, files: bool) -> Vec<Option<Row>> {
    let mut v = Vec::with_capacity(ROWS.len() + 2);
    for r in ROWS {
        let Some((act, key)) = r else {
            v.push(None);
            continue;
        };
        v.push(Some((act, Label::Key(key))));
        match act {
            Act::Launch("taskmgr.exe") if tasks => v.push(Some((Act::App(App::Tasks), Label::Name(App::Tasks)))),
            Act::Launch("explorer.exe") if files => v.push(Some((Act::App(App::Files), Label::Name(App::Files)))),
            _ => {}
        }
    }
    v
}

/// Label of row `i` of `rows` in the dock language (`""` for separators).
fn label(rows: &[Option<Row>], i: usize) -> &'static str {
    match rows.get(i).copied().flatten() {
        Some((_, Label::Key(key))) => t(key),
        Some((_, Label::Name(app))) => suite_links::name(app),
        None => "",
    }
}

/// Whether row `i` carries the Beta tag.
fn tagged(rows: &[Option<Row>], i: usize) -> bool {
    matches!(rows.get(i).copied().flatten(), Some((_, Label::Name(app))) if suite_links::beta(app))
}

pub(crate) struct StartPopup {
    /// The rows, built once when the popup opens (`row_list`).
    rows: Vec<Option<Row>>,
    hits: HitMap,
    /// Row chosen, run after closing.
    pending: Option<Act>,
    width: i32,
}

/// Creates the content (initial state). Labels follow the dock language (`i18n`; English in
/// preview mode, so the README screenshot does not depend on the machine).
pub(crate) fn new() -> Box<dyn Content> {
    suite_links::refresh();
    preview_suite(suite_links::usable(App::Tasks), suite_links::usable(App::Files))
}

/// The content with the suite entries given (also the golden's).
pub(crate) fn preview_suite(tasks: bool, files: bool) -> Box<dyn Content> {
    Box::new(StartPopup { rows: row_list(tasks, files), hits: HitMap::default(), pending: None, width: 0 })
}

/// Height taken by a separator row (physical px): space, 1 px line, space.
fn sep_h(s: &Surface) -> i32 {
    s.metrics().px(4) * 2 + s.metrics().px(1)
}

impl Content for StartPopup {
    fn kind(&self) -> PopupKind {
        PopupKind::Start
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let (inset, row, pad_text) = (m.px(8), m.px(34), m.px(12));
        let beta = t(k::COMMON_BETA);
        let tw = (0..self.rows.len())
            .map(|i| {
                let tag_w = if tagged(&self.rows, i) { suite_links::tag_width(s, beta) + m.px(12) } else { 0 };
                s.text_width(label(&self.rows, i), Font::Normal) + tag_w
            })
            .max()
            .unwrap_or(0);
        let w = (tw + 2 * pad_text + 2 * inset).clamp(m.px(190), m.px(300));
        self.width = w;
        self.hits.clear();
        let mut y = inset;
        for (i, r) in self.rows.iter().enumerate() {
            if r.is_some() {
                self.hits.add(i as u32 + 1, rect_wh(inset, y, w - 2 * inset, row));
                y += row;
            } else {
                y += sep_h(s);
            }
        }
        (w, y + inset)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let (inset, pad_text) = (m.px(8), m.px(12));
        let mut y = inset;
        for (i, r) in self.rows.iter().enumerate() {
            if r.is_none() {
                hline(s, inset + pad_text, self.width - inset - pad_text, y + m.px(4));
                y += sep_h(s);
                continue;
            }
            let Some(rc) = self.hits.rect(i as u32 + 1) else { continue };
            y = rc.bottom;
            let h = row_plate(s, rc, i as u32 + 1, hot, pressed);
            let ty = center_y(&rc, s.line_height(Font::Normal));
            s.text(label(&self.rows, i), rc.left + pad_text, ty, Font::Normal, WHITE, lift(TEXT_ALPHA, h));
            if tagged(&self.rows, i) {
                suite_links::paint_tag(s, rc, t(k::COMMON_BETA));
            }
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn focus_targets(&self) -> Vec<(u32, RECT)> {
        self.hits.entries().to_vec()
    }

    fn click(&mut self, id: u32) -> Reply {
        self.pending = self.rows.get((id as usize).wrapping_sub(1)).copied().flatten().map(|(act, _)| act);
        Reply::Close
    }

    fn after_close(&mut self) {
        match self.pending.take() {
            Some(Act::Launch(target)) => crate::ui::launch_target_str(target),
            Some(Act::Admin(tool)) => shell_thread::request(ShellRequest::LaunchAdmin(tool)),
            Some(Act::WinKey(vk)) => crate::ui::send_win_key(vk),
            Some(Act::SignOut) => power::run(PowerAction::SignOut),
            Some(Act::DockSettings) => crate::popup_power::open_settings_window(),
            Some(Act::App(App::Tasks)) => {
                suite_links::open_tasks(Show::default());
            }
            Some(Act::App(App::Files)) => {
                suite_links::open_files();
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_settings_follows_windows_settings() {
        let rows = row_list(true, true);
        let i = rows.iter().position(|r| matches!(r, Some((Act::DockSettings, _)))).expect("row");
        assert!(matches!(rows[i - 1], Some((Act::Launch("ms-settings:"), _))));
        let mut p = StartPopup { rows, hits: HitMap::default(), pending: None, width: 0 };
        assert_eq!(p.click(i as u32 + 1), Reply::Close);
        assert!(matches!(p.pending, Some(Act::DockSettings)));
    }

    #[test]
    fn start_keyboard_targets() {
        let mut p = preview_suite(false, false);
        // The id of a row is its place in the table plus one: the separators leave gaps, they are not targets.
        let want: Vec<u32> = ROWS.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i as u32 + 1).collect();
        assert_eq!(crate::popup::target_ids(&mut *p), want);
        assert!(want.len() > 3);
    }

    #[test]
    fn labels_equal_the_old_texts_in_both_languages() {
        // (row index, English, Italian) as the table held them before the keyed strings.
        let rows = row_list(false, false);
        let old: [(usize, &str, &str); 17] = [
            (0, "Installed apps", "App installate"),
            (1, "Power Options", "Opzioni spegnimento"),
            (2, "Event Viewer", "Visualizzatore eventi"),
            (3, "System", "Sistema"),
            (4, "Device Manager", "Gestione dispositivi"),
            (5, "Network Connections", "Connessioni di rete"),
            (6, "Disk Management", "Gestione disco"),
            (7, "Computer Management", "Gestione computer"),
            (8, "Terminal", "Terminale"),
            (9, "Terminal (admin)", "Terminale (admin)"),
            (11, "Task Manager", "Gestione attivit\u{e0}"),
            (12, "Settings", "Impostazioni"),
            (13, "Dock settings", "Impostazioni dock"),
            (14, "File Explorer", "Esplora file"),
            (15, "Search", "Cerca"),
            (16, "Run", "Esegui"),
            (18, "Sign out", "Disconnetti"),
        ];
        for (i, en, it) in old {
            assert_eq!(crate::i18n::with_italian(false, || label(&rows, i)), en);
            assert_eq!(crate::i18n::with_italian(true, || label(&rows, i)), it);
        }
        assert_eq!(label(&rows, 10), "");
        assert_eq!(label(&rows, 17), "");
    }

    #[test]
    fn start_rows_insert_after_their_twin() {
        let rows = row_list(true, true);
        let at = |f: &dyn Fn(&Option<Row>) -> bool| rows.iter().position(f).unwrap();
        let tm = at(&|r| matches!(r, Some((Act::Launch("taskmgr.exe"), _))));
        let fe = at(&|r| matches!(r, Some((Act::Launch("explorer.exe"), _))));
        assert!(matches!(rows[tm + 1], Some((Act::App(App::Tasks), _))));
        assert!(matches!(rows[fe + 1], Some((Act::App(App::Files), _))));
        assert_eq!(rows.len(), ROWS.len() + 2);
        assert_eq!(rows.iter().filter(|r| r.is_none()).count(), ROWS.iter().filter(|r| r.is_none()).count());
        let plain = row_list(false, false);
        assert_eq!(plain.len(), ROWS.len());
        for (a, b) in plain.iter().zip(ROWS.iter()) {
            assert_eq!(a.is_some(), b.is_some());
        }
        assert!(row_list(true, false).iter().all(|r| !matches!(r, Some((Act::App(App::Files), _)))));
    }

    #[test]
    fn app_rows_use_the_table_name() {
        let src = include_str!("popup_start.rs").replace("\r\n", "\n");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains("\"Tasks\"") && !code.contains("\"Folio\""));
    }

    #[test]
    fn menus_refresh_before_rows() {
        let src = include_str!("popup_start.rs").replace("\r\n", "\n");
        let body = &src[src.find("pub(crate) fn new()").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.find("suite_links::refresh()").unwrap() < body.find("usable(").unwrap());
    }
}
