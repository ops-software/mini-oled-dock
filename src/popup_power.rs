//! POWER popup: Sleep, Lock, Restart, Shut down, separator, "Settings" (closes the popup and opens the
//! Settings window, `settings_window`), "Start with Windows" (toggle, scheduled task: `autostart`), "Exit dock". A click runs the action immediately: the menu itself guards against accidental clicks
//! (user's choice, no confirmation). Actions in `power::run`.
//!
//! The items, labels, glyphs and the choice behind each one are shared (`pub(crate)` helpers below)
//! with the power flyout of the horizontal Start menu (`popup_startmenu`), which offers the same
//! entries except the autostart toggle.

use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
use std::sync::Arc;

use crate::glyphs;
use crate::log;
use crate::i18n::tr;
use crate::popup::{center_y, hline, lift, row_plate, toggle_switch, Content, HitMap, PopupKind, Reply};
use crate::power::{self, PowerAction};
use crate::surface::{rect_wh, Font, Surface, TEXT2_ALPHA, TEXT3_ALPHA, TEXT_ALPHA, WHITE};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub(crate) const ID_SLEEP: u32 = 1;
pub(crate) const ID_LOCK: u32 = 2;
pub(crate) const ID_RESTART: u32 = 3;
pub(crate) const ID_SHUTDOWN: u32 = 4;
pub(crate) const ID_EXIT: u32 = 5;
const ID_AUTOSTART: u32 = 6;
pub(crate) const ID_SETTINGS: u32 = 7;

/// Items above the separator (the system actions), in menu order.
pub(crate) const SYSTEM_ITEMS: [u32; 4] = [ID_SLEEP, ID_LOCK, ID_RESTART, ID_SHUTDOWN];
/// Items of the Start menu's power flyout below the separator (no autostart toggle there: its state
/// needs a schtasks query on open).
pub(crate) const DOCK_ITEMS: [u32; 2] = [ID_SETTINGS, ID_EXIT];

/// What choosing a power item does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Run the action after the popup closes (`power::run`).
    Run(PowerAction),
    /// Close the popup, then open the Settings window.
    Settings,
}

/// The choice behind item `id` (`None` for the autostart toggle and unknown ids).
pub(crate) fn choice(id: u32) -> Option<Choice> {
    Some(match id {
        ID_SLEEP => Choice::Run(PowerAction::Sleep),
        ID_LOCK => Choice::Run(PowerAction::Lock),
        ID_RESTART => Choice::Run(PowerAction::Restart),
        ID_SHUTDOWN => Choice::Run(PowerAction::Shutdown),
        ID_EXIT => Choice::Run(PowerAction::ExitDock),
        ID_SETTINGS => Choice::Settings,
        _ => return None,
    })
}

/// Glyph of item `id` (16x16 space).
pub(crate) fn shape(id: u32) -> fn(f32, f32) -> bool {
    match id {
        ID_SLEEP => glyphs::moon_shape,
        ID_LOCK => glyphs::lock_shape,
        ID_RESTART => glyphs::restart_shape,
        ID_SHUTDOWN => glyphs::power_shape,
        ID_SETTINGS => glyphs::gear_shape,
        ID_AUTOSTART => glyphs::startup_shape,
        _ => glyphs::exit_shape,
    }
}

/// Base text alpha of item `id` ("Exit dock" is dimmer).
pub(crate) fn alpha(id: u32) -> u32 {
    if id == ID_EXIT {
        TEXT2_ALPHA
    } else {
        TEXT_ALPHA
    }
}

/// Runs a power action (after the popup that offered it has closed).
pub(crate) fn run(a: PowerAction) {
    power::run(a);
}

/// Asks the UI thread to open (or activate) the Settings window. Any thread.
pub(crate) fn open_settings_window() {
    // SAFETY: PostMessageW is thread-safe; if the dock window no longer exists it just fails.
    let r = unsafe { PostMessageW(Some(crate::hwnd()), crate::msg::WM_DOCK_OPEN_SETTINGS, WPARAM(0), LPARAM(0)) };
    if let Err(e) = r {
        log!("settings: could not post the open request: {e}");
    }
}

/// Carries out a choice after the popup that offered it has closed.
pub(crate) fn run_choice(c: Choice) {
    match c {
        Choice::Run(a) => run(a),
        Choice::Settings => open_settings_window(),
    }
}

/// State of the "Start with Windows" toggle (shared with the threads that query/modify the task).
const AS_UNKNOWN: u8 = 0;
const AS_OFF: u8 = 1;
const AS_ON: u8 = 2;
const AS_BUSY: u8 = 3;

pub(crate) struct PowerPopup {
    hits: HitMap,
    /// Choice to carry out after closing.
    pending: Option<Choice>,
    /// Cached geometry: separator and padding (physical px).
    sep_y: i32,
    pad_text: i32,
    width: i32,
    /// Autostart state (updated by throwaway threads, never by the UI thread).
    autostart: Arc<AtomicU8>,
    /// Last state seen by `tick` (to redraw only when it changes).
    seen: u8,
    /// Update badge of the Settings row (`update::status().badge()`), re-read by `tick`.
    badge: u32,
    /// Preview only: a fixed badge that `tick` never overwrites.
    forced_badge: Option<u32>,
}

/// Text of the update badge on the Settings row: nothing for 0, the count, "9+" above 9.
pub(crate) fn settings_badge(count: u32) -> Option<String> {
    match count {
        0 => None,
        1..=9 => Some(count.to_string()),
        _ => Some("9+".to_string()),
    }
}

/// Orange dot that carries the update count `text`, its right edge at `right`, centred vertically
/// in `rc`. Also drawn on the Updates entry of the Settings window (`settings_view`).
pub(crate) fn paint_badge(s: &mut Surface, right: i32, rc: &windows::Win32::Foundation::RECT, text: &str) {
    let m = s.metrics();
    let tw = s.text_width(text, Font::Small);
    let bh = m.px(16);
    let bw = bh.max(tw + m.px(8));
    let left = right - bw;
    let top = rc.top + ((rc.bottom - rc.top) - bh) / 2;
    s.fill_rr(rect_wh(left, top, bw, bh), m.px(8), crate::render::ATTENTION_RGB, 255);
    let ty = center_y(&rect_wh(left, top, bw, bh), s.line_height(Font::Small));
    s.text(text, left + (bw - tw) / 2, ty, Font::Small, WHITE, TEXT_ALPHA);
}

/// Creates the content (initial state).
pub(crate) fn new() -> Box<dyn Content> {
    build(None)
}

fn build(forced_badge: Option<u32>) -> Box<dyn Content> {
    let autostart = Arc::new(AtomicU8::new(AS_UNKNOWN));
    // The schtasks.exe query can take hundreds of ms: keep it off the UI thread.
    let a = Arc::clone(&autostart);
    let spawned = std::thread::Builder::new().name("autostart-query".into()).spawn(move || {
        a.store(if crate::autostart::is_enabled() { AS_ON } else { AS_OFF }, SeqCst);
    });
    if let Err(e) = spawned {
        log!("power popup: autostart-query thread not started: {e}");
        autostart.store(AS_OFF, SeqCst);
    }
    Box::new(PowerPopup {
        hits: HitMap::default(),
        pending: None,
        sep_y: 0,
        pad_text: 0,
        width: 0,
        autostart,
        seen: AS_UNKNOWN,
        badge: forced_badge.unwrap_or_else(|| crate::update::status().badge()),
        forced_badge,
    })
}

/// Preview: the popup with a fixed update badge.
pub(crate) fn preview_with_badge(count: u32) -> Box<dyn Content> {
    build(Some(count))
}

/// Label of item `id` in the dock language.
pub(crate) fn label(id: u32) -> &'static str {
    match id {
        ID_SLEEP => tr("Sleep", "Sospendi"),
        ID_LOCK => tr("Lock", "Blocca"),
        ID_RESTART => tr("Restart", "Riavvia"),
        ID_SHUTDOWN => tr("Shut down", "Arresta il sistema"),
        ID_AUTOSTART => tr("Start with Windows", "Avvia con Windows"),
        ID_SETTINGS => tr("Settings", "Impostazioni"),
        _ => tr("Exit dock", "Esci dal dock"),
    }
}

impl Content for PowerPopup {
    fn kind(&self) -> PopupKind {
        PopupKind::Power
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let (inset, row) = (m.px(8), m.px(34));
        let w = m.px(244);
        self.width = w;
        self.pad_text = m.px(12);
        self.hits.clear();
        let mut y = inset;
        for id in SYSTEM_ITEMS {
            self.hits.add(id, rect_wh(inset, y, w - 2 * inset, row));
            y += row;
        }
        self.sep_y = y + m.px(6);
        y += m.px(6) * 2 + m.px(1);
        self.hits.add(ID_SETTINGS, rect_wh(inset, y, w - 2 * inset, row));
        y += row;
        self.hits.add(ID_AUTOSTART, rect_wh(inset, y, w - 2 * inset, row));
        y += row;
        self.hits.add(ID_EXIT, rect_wh(inset, y, w - 2 * inset, row));
        y += row;
        (w, y + inset)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let inset = s.metrics().px(8);
        hline(s, inset + self.pad_text, self.width - inset - self.pad_text, self.sep_y);
        for id in [ID_SLEEP, ID_LOCK, ID_RESTART, ID_SHUTDOWN, ID_SETTINGS, ID_AUTOSTART, ID_EXIT] {
            let Some(rc) = self.hits.rect(id) else { continue };
            let h = row_plate(s, rc, id, hot, pressed);
            let (txt, alpha) = (label(id), alpha(id));
            let font = Font::Normal;
            let y = center_y(&rc, s.line_height(font));
            // Icon in a 16 px slot at the text column, label after it (same alignment as the audio devices).
            let m = s.metrics();
            let icon = m.px(16);
            let shape = shape(id);
            s.glyph(rc.left + self.pad_text, rc.top + ((rc.bottom - rc.top) - icon) / 2, icon, lift(alpha, h), &shape);
            s.text(txt, rc.left + self.pad_text + icon + m.px(8), y, font, WHITE, lift(alpha, h));
            if id == ID_SETTINGS {
                let cw = s.text_width("\u{203A}", font);
                let cx = rc.right - self.pad_text - cw;
                s.text("\u{203A}", cx, y, font, WHITE, lift(TEXT3_ALPHA, h));
                if let Some(text) = settings_badge(self.badge) {
                    // Right-aligned before the chevron.
                    paint_badge(s, cx - s.metrics().px(6), &rc, &text);
                }
            }
            if id == ID_AUTOSTART {
                let st = self.autostart.load(SeqCst);
                let m = s.metrics();
                let (sw_w, sw_h) = (m.px(32), m.px(18));
                toggle_switch(s, rc.right - m.px(12) - sw_w, rc.top + (rc.bottom - rc.top - sw_h) / 2, st == AS_ON);
            }
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn clickable(&self, id: u32) -> bool {
        id != ID_AUTOSTART || matches!(self.autostart.load(SeqCst), AS_ON | AS_OFF)
    }

    fn tick(&mut self) -> Reply {
        let cur = self.autostart.load(SeqCst);
        let badge = self.forced_badge.unwrap_or_else(|| crate::update::status().badge());
        if cur != self.seen || badge != self.badge {
            self.seen = cur;
            self.badge = badge;
            return Reply::Redraw;
        }
        Reply::None
    }

    fn click(&mut self, id: u32) -> Reply {
        if id == ID_AUTOSTART {
            return self.toggle_autostart();
        }
        match choice(id) {
            Some(c) => {
                self.pending = Some(c);
                Reply::Close
            }
            None => Reply::None,
        }
    }

    fn after_close(&mut self) {
        if let Some(c) = self.pending.take() {
            run_choice(c);
        }
    }
}

impl PowerPopup {
    /// Creates or removes the task on a throwaway thread; the popup stays open and `tick` redraws.
    fn toggle_autostart(&mut self) -> Reply {
        let want_on = match self.autostart.load(SeqCst) {
            AS_OFF => true,
            AS_ON => false,
            _ => return Reply::None, // unknown or already busy
        };
        let cur = if want_on { AS_OFF } else { AS_ON };
        if self.autostart.compare_exchange(cur, AS_BUSY, SeqCst, SeqCst).is_err() {
            return Reply::None;
        }
        let a = Arc::clone(&self.autostart);
        let spawned = std::thread::Builder::new().name("autostart-set".into()).spawn(move || {
            let r = if want_on {
                match crate::autostart::enable(false).error {
                    None => Ok(()),
                    Some(e) => Err(e.to_string()),
                }
            } else {
                crate::autostart::disable()
            };
            if let Err(e) = &r {
                log!("autostart: operation failed: {e}");
            }
            // The displayed state is always the real state of the task, even after an error.
            a.store(if crate::autostart::is_enabled() { AS_ON } else { AS_OFF }, SeqCst);
        });
        if let Err(e) = spawned {
            log!("power popup: autostart-set thread not started: {e}");
            self.autostart.store(cur, SeqCst);
        }
        Reply::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_closes_the_popup_then_opens_the_window() {
        let mut p = PowerPopup {
            hits: HitMap::default(),
            pending: None,
            sep_y: 0,
            pad_text: 0,
            width: 0,
            autostart: Arc::new(AtomicU8::new(AS_OFF)),
            seen: AS_OFF,
            badge: 0,
            forced_badge: Some(0),
        };
        assert_eq!(p.click(ID_SETTINGS), Reply::Close);
        assert_eq!(p.pending, Some(Choice::Settings));
        assert_eq!(p.click(ID_LOCK), Reply::Close);
        assert_eq!(p.pending, Some(Choice::Run(PowerAction::Lock)));
    }

    #[test]
    fn settings_badge_text() {
        assert_eq!(settings_badge(0), None);
        assert_eq!(settings_badge(1).as_deref(), Some("1"));
        assert_eq!(settings_badge(12).as_deref(), Some("9+"));
    }
}
