//! The toolbar: the Processes tab, the status, the search field and the More button, all of the kit's Toolbar. Filled by
//! TA-T1a (T1.4) and TA-T5a (T5.1).

use oled_ui::model::FgRole;
use oled_ui::widgets::{FieldKind, FieldOut, PageTabs, TextField, Toolbar, ToolbarButton, ToolbarOut};
use oled_ui::{CommandId, Event, EventCx, Glyph, LayoutCx, PaintCx, RectP, TypeRole, UiLang, Widget, WidgetId};

use super::commands::{MORE, PAGE_PROCESSES};
use super::screen::{TasksScreen, CHROME};
use crate::app::sampling::Status;
use crate::strings::{k, t};

/// The search field's widget id (the toolbar's buttons and tab are nodes of CHROME).
pub const SEARCH: WidgetId = WidgetId(5);
/// The search field's width, logical px.
const FIELD_W: i32 = 200;
/// The kit's page tab and toolbar measures the More button's place follows (logical px).
const TAB_PAD: i32 = 8;
const MARGIN: i32 = 16;
const GROUP_GAP: i32 = 12;
const BUTTON_SIDE: i32 = 28;

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
    /// (matching, all) while a query stands: it replaces the status text.
    count: Option<(usize, usize)>,
    toolbar: Option<Toolbar>,
    /// What the search field holds (a mirror: the kit's field is only reachable with a mutable borrow).
    typed: String,
    more_rect: RectP,
    /// The language the toolbar's texts were written in.
    lang: UiLang,
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

/// "3 of 41".
pub fn count_text(lang: UiLang, matching: usize, total: usize) -> String {
    oled_sys::i18n::subst(
        &t(lang, k::TASKS_SEARCH_COUNT),
        &[("n", &matching.to_string()), ("total", &total.to_string())],
    )
}

/// The toolbar of a language: one page tab, the More button and the search field holding `query`.
fn make_toolbar(lang: UiLang, query: &str) -> Toolbar {
    let tab = t(lang, k::TASKS_TAB_PROCESSES).into_owned();
    let tip = format!("{} (F10)", t(lang, k::TASKS_MORE_OPTIONS));
    let more = ToolbarButton { command: MORE, glyph: Glyph::More, tip: tip.into(), toggled: None };
    let mut field = TextField::new(SEARCH, FieldKind::Search);
    field.set_placeholder(t(lang, k::TASKS_SEARCH_PLACEHOLDER).into_owned().into());
    field.set_label(t(lang, k::TASKS_SEARCH_LABEL).into_owned().into());
    field.set_clear_button(true);
    field.set_width(FIELD_W);
    if !query.is_empty() {
        // A language switch keeps what was typed (there is no event context here to set it the usual way).
        field.preview(query, false, None, None, None);
    }
    Toolbar::new(CHROME, Some(PageTabs::new(CHROME, vec![tab.into()])), vec![more], Some(field))
}

impl Chrome {
    /// The sampler's status changed (T1.4).
    pub fn set_status(&mut self, cx: &mut EventCx, s: Status) {
        if self.status == Some(s) {
            return;
        }
        self.status = Some(s);
        cx.relayout();
    }

    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// The search's count changed: it stands in the status place while a query stands (T5.1).
    pub fn set_count(&mut self, cx: &mut EventCx, count: Option<(usize, usize)>) {
        if self.count == count {
            return;
        }
        self.count = count;
        cx.relayout();
    }

    /// The text and colour in the status place.
    fn status_line(&self, lang: UiLang) -> Option<(String, FgRole)> {
        match (self.count, self.status) {
            (Some((n, total)), _) => Some((count_text(lang, n, total), FgRole::Secondary)),
            (None, Some(s)) => Some(status_text(lang, s)),
            (None, None) => None,
        }
    }

    fn field(&mut self) -> Option<&mut TextField> {
        self.toolbar.as_mut()?.field_mut()
    }

    /// Whether an IME composition is open in the field.
    pub fn composing(&mut self) -> bool {
        self.field().is_some_and(|f| f.is_composing())
    }

    /// The field takes the focus (T5.3); false before the first layout.
    pub fn focus_field(&mut self, cx: &mut EventCx) -> bool {
        let Some(f) = self.field() else { return false };
        cx.set_focus(f.node_id());
        true
    }

    /// The field takes the focus with its text selected, as Ctrl+F does.
    pub fn select_field(&mut self, cx: &mut EventCx) {
        let Some(f) = self.field() else { return };
        let text = f.text().to_string();
        f.set_text(cx, &text, true);
        cx.set_focus(f.node_id());
    }

    /// Empties the field.
    pub fn clear_field(&mut self, cx: &mut EventCx) {
        self.typed.clear();
        if let Some(f) = self.field() {
            f.set_text(cx, "", false);
        }
    }

    /// The field takes the focus and gets one more character at the end; its new text.
    pub fn type_into_field(&mut self, cx: &mut EventCx, c: char) -> Option<String> {
        let mut text = self.typed.clone();
        text.push(c);
        let f = self.toolbar.as_mut()?.field_mut()?;
        f.set_text(cx, &text, false);
        cx.set_focus(f.node_id());
        self.typed.clone_from(&text);
        Some(text)
    }

    /// The text in the search field.
    pub fn query(&self) -> &str {
        &self.typed
    }

    /// (matching, all) while the count stands in the status place.
    pub fn counted(&self) -> Option<(usize, usize)> {
        self.count
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
        if self.toolbar.is_none() || self.lang != lang {
            // The first layout, or a language switch: the texts are written again, the query stays.
            self.lang = lang;
            self.toolbar = Some(make_toolbar(lang, &self.typed));
        }
        let status = self.status_line(lang);
        let Some(toolbar) = self.toolbar.as_mut() else { return };
        toolbar.set_status(status.map(|(s, fg)| (s.into(), fg)));
        toolbar.layout(cx, rect);
        // The More button follows the one tab (the kit's Toolbar lays the tabs out and then the buttons).
        let font = cx.font(TypeRole::Body);
        let tab = t(lang, k::TASKS_TAB_PROCESSES);
        let tab_w = cx.text().width(&tab, font) + 2 * cx.px(TAB_PAD);
        let side = cx.px(BUTTON_SIDE);
        self.more_rect =
            RectP::wh(rect.l + cx.px(MARGIN) + tab_w + cx.px(GROUP_GAP), rect.t + (rect.h() - side) / 2, side, side);
    }

    fn paint(&self, cx: &mut PaintCx) {
        if let Some(toolbar) = &self.toolbar {
            toolbar.paint(cx);
        }
    }

    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<ChromeOut> {
        match self.toolbar.as_mut()?.event(cx, ev)? {
            ToolbarOut::Command(id) => Some(ChromeOut::Command(id)),
            ToolbarOut::Tabs(_) => Some(ChromeOut::Command(PAGE_PROCESSES)),
            ToolbarOut::Field(FieldOut::Changed) => {
                self.typed = self.toolbar.as_mut()?.field_mut()?.text().to_string();
                Some(ChromeOut::Query(self.typed.clone()))
            }
            ToolbarOut::Field(_) => None,
        }
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
