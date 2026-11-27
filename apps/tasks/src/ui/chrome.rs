//! The toolbar: the Processes tab, the search field, the status and the More button. Filled by TA-T1a (T1.4) and
//! TA-T5a (T5.1).

use oled_ui::model::FgRole;
use oled_ui::widgets::{ButtonOut, IconButton, TextLabel};
use oled_ui::{
    CommandId, Event, EventCx, Glyph, LayoutCx, Node, NodeId, PaintCx, RectP, Role, TypeRole, UiLang, Widget, WidgetId,
};

use super::commands::{MORE, PAGE_PROCESSES};
use super::screen::{TasksScreen, CHROME};
use crate::app::sampling::Status;
use crate::strings::{self, k, t};

/// The status text's widget id (an inert label; the More button is the toolbar's own widget, CHROME, so its clicks reach
/// chrome.event).
pub const STATUS_LABEL: WidgetId = WidgetId(5);
/// The toolbar's group node: slot 1 of CHROME (slot 0 is the More button).
const GROUP_SLOT: u32 = 1;

/// What the toolbar tells the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromeOut {
    Command(CommandId),
    /// The search field's text changed.
    Query(String),
}

#[derive(Default)]
pub struct Chrome {
    rect: RectP,
    status: Option<Status>,
    label: Option<TextLabel>,
    more: Option<IconButton>,
    more_rect: RectP,
    /// The language the tooltip and the status text were written in.
    lang: UiLang,
}

/// The status text as a label.
fn status_label(lang: UiLang, s: Status) -> TextLabel {
    let (text, fg) = status_text(lang, s);
    TextLabel::new(STATUS_LABEL, text.into(), TypeRole::Caption, fg)
}

/// "2", "0.5" (decimal comma in Italian).
fn seconds(ms: u32, lang: UiLang) -> String {
    let sign = if lang.italian { ',' } else { '.' };
    if ms.is_multiple_of(1000) {
        (ms / 1000).to_string()
    } else {
        format!("{}{sign}{}", ms / 1000, (ms % 1000) / 100)
    }
}

/// The toolbar's status words and their colour.
pub fn status_text(lang: UiLang, s: Status) -> (String, FgRole) {
    let every = |key, ms| oled_sys::i18n::subst(&t(lang, key), &[("s", &seconds(ms, lang))]);
    match s {
        Status::Every { period_ms } => (every(k::TASKS_STATUS_EVERY, period_ms), FgRole::Secondary),
        Status::EveryIdle { period_ms } => (every(k::TASKS_STATUS_EVERY_IDLE, period_ms), FgRole::Secondary),
        Status::Paused => (t(lang, k::TASKS_STATUS_PAUSED).into_owned(), FgRole::Attention),
    }
}

impl Chrome {
    /// The sampler's status changed (T1.4, then T5.1).
    pub fn set_status(&mut self, cx: &mut EventCx, s: Status) {
        if self.status == Some(s) {
            return;
        }
        self.status = Some(s);
        self.label = Some(status_label(cx.lang(), s));
        cx.relayout();
    }

    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// The More button's plate, for the menu that opens under it.
    pub fn more_rect(&self) -> RectP {
        self.more_rect
    }
}

impl Widget for Chrome {
    type Out = ChromeOut;

    fn layout(&mut self, cx: &mut LayoutCx, rect: RectP) {
        self.rect = rect;
        let lang = cx.lang();
        if self.lang != lang {
            // A language switch: the tooltip and the status words are written again.
            self.lang = lang;
            self.more = None;
            self.label = self.status.map(|s| status_label(lang, s));
        }
        let name = strings::app_name(lang);
        cx.push(Node::new(NodeId::new(CHROME, GROUP_SLOT), Role::Toolbar, rect).label(name));
        let side = cx.px(28);
        let more_rect = RectP::wh(rect.r - cx.px(16) - side, rect.t + cx.px(8), side, side);
        self.more_rect = more_rect;
        let more = self.more.get_or_insert_with(|| {
            let tip = format!("{} (F10)", lang.kit(oled_ui::strings::k::KIT_MORE));
            IconButton::new(CHROME, Glyph::More, tip.into())
        });
        more.layout(cx, more_rect);
        if let Some(label) = self.label.as_mut() {
            let w = label.preferred(cx).w;
            let right = more_rect.l - cx.px(8);
            label.layout(cx, RectP { l: (right - w).max(rect.l), t: rect.t, r: right, b: rect.b });
        }
    }

    fn paint(&self, cx: &mut PaintCx) {
        if let Some(label) = &self.label {
            label.paint(cx);
        }
        if let Some(more) = &self.more {
            more.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ChromeOut> {
        self.more.as_mut()?.event(cx, ev).map(|ButtonOut::Clicked| ChromeOut::Command(MORE))
    }
}

/// PAGE_PROCESSES, MORE.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, id: CommandId) {
    match id {
        // The beta has one page: Ctrl+1 is already there.
        PAGE_PROCESSES => {}
        MORE => super::more_menu::open(s, cx),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_words() {
        assert_eq!(seconds(2000, UiLang::EN), "2");
        assert_eq!(seconds(500, UiLang::EN), "0.5");
        assert_eq!(seconds(500, UiLang::IT), "0,5");
        let (text, fg) = status_text(UiLang::EN, Status::Every { period_ms: 2000 });
        assert_eq!((text.as_str(), fg), ("Every 2 s", FgRole::Secondary));
        let (text, _) = status_text(UiLang::EN, Status::EveryIdle { period_ms: 2000 });
        assert_eq!(text, "Every 2 s \u{b7} idle");
        let (text, fg) = status_text(UiLang::EN, Status::Paused);
        assert_eq!((text.as_str(), fg), ("Paused", FgRole::Attention));
    }
}
