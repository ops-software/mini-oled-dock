//! The error band (a failed snapshot: its text and Open Windows Task Manager, in the verdict slot) and the notice line
//! above the window's bottom (the Updated notice). Filled by TA-T1a (T1.4, the band) and TA-SUITE (TS3, the line).

use oled_ui::model::FgRole;
use oled_ui::widgets::{Button, ButtonKind, ButtonOut, TextLabel};
use oled_ui::{Event, EventCx, LayoutCx, PaintCx, RectP, TypeRole, UiLang, Widget, WidgetId};

use super::screen::NOTICES;
use crate::strings::{k, t};

/// The error band's text label (inert); its button is the notices' own widget, NOTICES, so its clicks reach
/// Notices::event.
pub const BAND_LABEL: WidgetId = WidgetId(6);

/// What the notices tell the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeOut {
    /// The error band's button: open Windows' Task Manager.
    OpenTaskMgr,
}

/// The band's second text line, when the text needs two (its own node id, beside BAND_LABEL).
const BAND_LABEL_2: WidgetId = WidgetId(BAND_LABEL.0 | 0x1_0000);

#[derive(Default)]
pub struct Notices {
    band: RectP,
    line: RectP,
    error: Option<u32>,
    /// The language the band's texts were written in.
    lang: UiLang,
    text: String,
    label: Option<TextLabel>,
    label2: Option<TextLabel>,
    button: Option<Button>,
    /// The last layout had to cut a line of the text short.
    cut: bool,
}

impl Notices {
    /// The failed snapshot's code, or None when the list reads again. A change lays the window out again (the band takes
    /// room under the toolbar).
    pub fn set_error(&mut self, cx: &mut EventCx, code: Option<u32>) {
        if self.error == code {
            return;
        }
        self.error = code;
        self.build(cx.lang());
        cx.relayout();
    }

    /// The band's text and button in `lang`.
    fn build(&mut self, lang: UiLang) {
        self.lang = lang;
        self.label2 = None;
        self.text = self.error.map_or_else(String::new, |c| {
            oled_sys::i18n::subst(&t(lang, k::TASKS_ERROR_READ), &[("code", &format!("{c:#x}"))])
        });
        self.label =
            self.error.map(|_| TextLabel::new(BAND_LABEL, self.text.clone().into(), TypeRole::Body, FgRole::Danger));
        self.button =
            self.error.map(|_| Button::new(NOTICES, t(lang, k::TASKS_ERROR_READ_BUTTON), ButtonKind::Secondary));
    }

    /// True when the last layout cut a line of the band's text short.
    pub fn text_cut(&self) -> bool {
        self.cut
    }

    /// Laid out only while View::read_error is Some. The text takes one line when it fits beside the button, else two
    /// lines of the smaller type, split at a space.
    pub fn error_band(&mut self, cx: &mut LayoutCx, r: RectP) {
        self.band = r;
        if self.error.is_some() && self.lang != cx.lang() {
            self.build(cx.lang());
        }
        let h = cx.px(28);
        let left = r.l + cx.px(30);
        let mut right = r.r - cx.px(16);
        if let Some(b) = self.button.as_mut() {
            let w = b.preferred(cx).w;
            let rect = RectP::wh(right - w, r.t + (r.h() - h) / 2, w, h);
            b.layout(cx, rect);
            right = rect.l - cx.px(12);
        }
        let right = right.max(left);
        let avail = right - left;
        self.cut = false;
        let body = cx.font(TypeRole::Body);
        if cx.text().width(&self.text, body) <= avail {
            self.label2 = None;
            if let Some(l) = self.label.as_mut() {
                l.layout(cx, RectP { l: left, t: r.t, r: right, b: r.b });
            }
            return;
        }
        // Two lines of Caption inside the band.
        let small = cx.font(TypeRole::Caption);
        let split = self
            .text
            .match_indices(' ')
            .map(|(i, _)| i)
            .take_while(|&i| cx.text().width(&self.text[..i], small) <= avail)
            .last();
        let (first, second) = match split {
            Some(i) => (self.text[..i].to_string(), self.text[i + 1..].to_string()),
            None => (self.text.clone(), String::new()),
        };
        self.cut = cx.text().width(&first, small) > avail || cx.text().width(&second, small) > avail;
        let line = cx.px(TypeRole::Caption.spec().line);
        let top = r.t + (r.h() - 2 * line) / 2;
        let danger = |id, s: String| TextLabel::new(id, s.into(), TypeRole::Caption, FgRole::Danger);
        let (mut a, mut b) = (danger(BAND_LABEL, first), danger(BAND_LABEL_2, second));
        a.layout(cx, RectP { l: left, t: top, r: right, b: top + line });
        b.layout(cx, RectP { l: left, t: top + line, r: right, b: top + 2 * line });
        self.label = Some(a);
        self.label2 = Some(b);
    }

    pub fn paint_error_band(&self, cx: &mut PaintCx) {
        let r = self.band;
        if r.is_empty() || !cx.visible(r) {
            return;
        }
        let d = cx.px(6);
        let dot = RectP::wh(r.l + cx.px(16), r.t + (r.h() - d) / 2, d, d);
        let ink = cx.tokens().color.accent_danger;
        cx.fill(dot, d / 2, ink);
        if let Some(l) = &self.label {
            l.paint(cx);
        }
        if let Some(l) = &self.label2 {
            l.paint(cx);
        }
        if let Some(b) = &self.button {
            b.paint(cx);
        }
    }

    /// The band's button.
    pub fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Option<NoticeOut> {
        self.button.as_mut()?.event(cx, ev).map(|ButtonOut::Clicked| NoticeOut::OpenTaskMgr)
    }

    /// True while a notice shows (the line takes STATUS_H). TS3. Stub: false.
    pub fn line_shown(&self) -> bool {
        false
    }

    /// TS3. Stub: the rect only.
    pub fn line(&mut self, cx: &mut LayoutCx, r: RectP) {
        let _ = cx;
        self.line = r;
    }

    /// TS3. Stub: nothing.
    pub fn paint_line(&self, cx: &mut PaintCx) {
        let _ = cx;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use crate::app::args::PreviewLang;
    use crate::app::exec::RecordingExecutor;
    use crate::app::launch::RecordingLauncher;
    use crate::ui::scene::{self, Scene};

    fn open(lang: PreviewLang, dpi: u32) -> oled_ui::harness::Harness<crate::ui::screen::TasksScreen> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t1_read_error.json");
        let scene = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
        let (launcher, exec) = (Arc::new(RecordingLauncher::default()), Arc::new(RecordingExecutor::default()));
        scene::open(&scene, lang, dpi, launcher, exec).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The code of the failure is the part that matters: it shows in both languages at every scale, on two lines when
    /// one does not fit beside the button.
    #[test]
    fn the_error_text_is_not_cut() {
        for lang in [PreviewLang::En, PreviewLang::It] {
            for dpi in [96, 120, 144, 192] {
                let h = open(lang, dpi);
                assert!(!h.screen().notices.text_cut(), "{lang:?} at {dpi}: the text is cut");
                assert!(h.a11y_dump().contains("0xc0000004"), "{lang:?} at {dpi}: the code is not shown");
                h.assert_named();
            }
        }
    }
}
