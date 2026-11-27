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
use oled_sys::i18n::S;

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
}

const VK_S: VIRTUAL_KEY = VIRTUAL_KEY(0x53);
const VK_R: VIRTUAL_KEY = VIRTUAL_KEY(0x52);

/// One row: action and label (a key of the string table).
type Row = (Act, S);

/// Rows in the order of the native Win+X menu (`None` = separator). The row id is its index + 1.
const ROWS: [Option<Row>; 19] = [
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

/// Label of row `i` in the dock language (`""` for separators).
fn label(i: usize) -> &'static str {
    ROWS.get(i).copied().flatten().map_or("", |(_, key)| t(key))
}

pub(crate) struct StartPopup {
    hits: HitMap,
    /// Row chosen, run after closing.
    pending: Option<Act>,
    width: i32,
}

/// Creates the content (initial state). Labels follow the dock language (`i18n`; English in
/// preview mode, so the README screenshot does not depend on the machine).
pub(crate) fn new() -> Box<dyn Content> {
    Box::new(StartPopup { hits: HitMap::default(), pending: None, width: 0 })
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
        let tw = (0..ROWS.len()).map(|i| s.text_width(label(i), Font::Normal)).max().unwrap_or(0);
        let w = (tw + 2 * pad_text + 2 * inset).clamp(m.px(190), m.px(300));
        self.width = w;
        self.hits.clear();
        let mut y = inset;
        for (i, r) in ROWS.iter().enumerate() {
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
        for (i, r) in ROWS.iter().enumerate() {
            if r.is_none() {
                hline(s, inset + pad_text, self.width - inset - pad_text, y + m.px(4));
                y += sep_h(s);
                continue;
            }
            let Some(rc) = self.hits.rect(i as u32 + 1) else { continue };
            y = rc.bottom;
            let h = row_plate(s, rc, i as u32 + 1, hot, pressed);
            let ty = center_y(&rc, s.line_height(Font::Normal));
            s.text(label(i), rc.left + pad_text, ty, Font::Normal, WHITE, lift(TEXT_ALPHA, h));
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn focus_targets(&self) -> Vec<(u32, RECT)> {
        self.hits.entries().to_vec()
    }

    fn click(&mut self, id: u32) -> Reply {
        self.pending = ROWS.get((id as usize).wrapping_sub(1)).copied().flatten().map(|(act, _)| act);
        Reply::Close
    }

    fn after_close(&mut self) {
        match self.pending.take() {
            Some(Act::Launch(target)) => crate::ui::launch_target_str(target),
            Some(Act::Admin(tool)) => shell_thread::request(ShellRequest::LaunchAdmin(tool)),
            Some(Act::WinKey(vk)) => crate::ui::send_win_key(vk),
            Some(Act::SignOut) => power::run(PowerAction::SignOut),
            Some(Act::DockSettings) => crate::popup_power::open_settings_window(),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_settings_follows_windows_settings() {
        let i = ROWS.iter().position(|r| matches!(r, Some((Act::DockSettings, _)))).expect("row");
        assert!(matches!(ROWS[i - 1], Some((Act::Launch("ms-settings:"), _))));
        let mut p = StartPopup { hits: HitMap::default(), pending: None, width: 0 };
        assert_eq!(p.click(i as u32 + 1), Reply::Close);
        assert!(matches!(p.pending, Some(Act::DockSettings)));
    }

    #[test]
    fn start_keyboard_targets() {
        let mut p = new();
        // The id of a row is its place in the table plus one: the separators leave gaps, they are not targets.
        let want: Vec<u32> = ROWS.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i as u32 + 1).collect();
        assert_eq!(crate::popup::target_ids(&mut *p), want);
        assert!(want.len() > 3);
    }

    #[test]
    fn labels_equal_the_old_texts_in_both_languages() {
        // (row index, English, Italian) as the table held them before the keyed strings.
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
            assert_eq!(crate::i18n::with_italian(false, || label(i)), en);
            assert_eq!(crate::i18n::with_italian(true, || label(i)), it);
        }
        assert_eq!(label(10), "");
        assert_eq!(label(17), "");
    }
}
