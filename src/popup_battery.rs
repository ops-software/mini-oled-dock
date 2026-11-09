//! BATTERY popup: header with the battery glyph (same layers as the bar), charge and state, details
//! (time remaining on battery, active power plan), "Power & battery settings" link
//! (`ms-settings:batterysaver`, de-elevated through the shell thread). Data: `battery::BatteryStatus`
//! (last known on open, then `DataEvent::Battery` from the samples of the visible bar).

use crate::battery::{BatteryStatus, ChargeState, PowerMode};
use crate::glyphs;
use crate::popup::{center_y, hline, launch, lift, row_plate, Content, DataEvent, HitMap, PopupKind, Reply};
use crate::render::batt_tint_rgb;
use crate::surface::{rect_wh, Font, Surface, PANEL_PAD, TEXT2_ALPHA, TEXT3_ALPHA, TEXT_ALPHA, TEXT_COL, WHITE};

const ID_SETTINGS: u32 = 1;

pub(crate) struct BatteryPopup {
    status: BatteryStatus,
    mode: Option<PowerMode>,
    open_settings: bool,
    // Cached geometry (physical px).
    hits: HitMap,
    width: i32,
    pad: i32,
    y_head: i32,
    head_h: i32,
    sep1: i32,
    y_rows: i32,
    row_h: i32,
    sep2: i32,
}

/// Creates the content with the last known status and the active power plan (`None` = unknown).
pub(crate) fn new(status: BatteryStatus, mode: Option<PowerMode>) -> Box<dyn Content> {
    Box::new(BatteryPopup {
        status,
        mode,
        open_settings: false,
        hits: HitMap::default(),
        width: 0,
        pad: 0,
        y_head: 0,
        head_h: 0,
        sep1: 0,
        y_rows: 0,
        row_h: 0,
        sep2: 0,
    })
}

/// Header lines: charge (or "No battery") and what the battery is doing.
fn title(s: &BatteryStatus) -> (String, String) {
    let tr = crate::i18n::tr;
    if !s.present {
        return (tr("No battery", "Nessuna batteria").to_string(), String::new());
    }
    let state = match s.state() {
        ChargeState::OnBattery => tr("On battery", "A batteria"),
        ChargeState::Charging => tr("Charging", "In carica"),
        ChargeState::Full => tr("Fully charged", "Completamente carica"),
        ChargeState::PluggedIn => tr("Plugged in", "Collegato alla corrente"),
    };
    (format!("{}%", s.percent), state.to_string())
}

/// Detail rows (label, value); only what Windows reports.
fn rows(s: &BatteryStatus, mode: Option<PowerMode>) -> Vec<(&'static str, String)> {
    let tr = crate::i18n::tr;
    let mut v = Vec::new();
    if let Some(secs) = s.seconds_left {
        v.push((tr("Time remaining", "Tempo rimanente"), crate::battery::format_remaining(secs)));
    }
    if let Some(m) = mode {
        let name = match m {
            PowerMode::PowerSaver => tr("Power saver", "Risparmio energia"),
            PowerMode::Balanced => tr("Balanced", "Bilanciato"),
            PowerMode::HighPerformance => tr("High performance", "Prestazioni elevate"),
        };
        v.push((tr("Power plan", "Piano di alimentazione"), name.to_string()));
    }
    v
}

/// Draws the battery glyph (same layers as the bar) in a `size` square at `(x, y)`.
fn battery_glyph(s: &mut Surface, x: i32, y: i32, size: i32, st: &BatteryStatus) {
    if !st.present {
        s.glyph(x, y, size, TEXT_ALPHA, &glyphs::battery_off_shape);
        return;
    }
    let tint = batt_tint_rgb(st.tint());
    let (pct, bolt) = (st.percent, st.charging);
    s.glyph_rgb(x, y, size, tint, TEXT_ALPHA, &move |u, v| glyphs::battery_fill_shape(u, v, pct, bolt));
    s.glyph(x, y, size, TEXT_ALPHA, &glyphs::battery_outline_shape);
    if bolt {
        s.glyph(x, y, size, 255, &glyphs::bolt_shape);
    }
}

impl Content for BatteryPopup {
    fn kind(&self) -> PopupKind {
        PopupKind::Battery
    }

    fn size(&mut self, s: &Surface) -> (i32, i32) {
        let m = s.metrics();
        let p = |v: i32| m.px(v);
        self.width = p(280);
        self.pad = p(PANEL_PAD);
        self.hits.clear();
        let mut y = self.pad;
        self.y_head = y;
        self.head_h = s.line_height(Font::Bold) + s.line_height(Font::Small) + p(8);
        y += self.head_h;
        self.row_h = p(26);
        let n = rows(&self.status, self.mode).len() as i32;
        if n > 0 {
            y += p(6);
            self.sep1 = y;
            y += p(1) + p(6);
            self.y_rows = y;
            y += self.row_h * n;
        }
        y += p(6);
        self.sep2 = y;
        y += p(1) + p(6);
        let inset = p(8);
        self.hits.add(ID_SETTINGS, rect_wh(inset, y, self.width - 2 * inset, p(34)));
        y += p(34) + inset;
        (self.width, y)
    }

    fn paint(&self, s: &mut Surface, hot: Option<u32>, pressed: Option<u32>) {
        s.panel();
        let m = s.metrics();
        let col = m.px(TEXT_COL);
        // Header: 24 px glyph, charge (bold) and state (small) on two lines.
        let gsz = m.px(24);
        let gy = self.y_head + (self.head_h - gsz) / 2;
        battery_glyph(s, col - m.px(4), gy, gsz, &self.status);
        let tx = col - m.px(4) + gsz + m.px(12);
        let (name, sub) = title(&self.status);
        let lb = s.line_height(Font::Bold);
        let ls = s.line_height(Font::Small);
        let ty = self.y_head + (self.head_h - lb - ls) / 2;
        let name = s.ellipsize(&name, Font::Bold, self.width - col - tx);
        s.text(&name, tx, ty, Font::Bold, WHITE, TEXT_ALPHA);
        s.text(&sub, tx, ty + lb, Font::Small, WHITE, TEXT3_ALPHA);

        let ln = s.line_height(Font::Normal);
        let details = rows(&self.status, self.mode);
        if !details.is_empty() {
            hline(s, col, self.width - col, self.sep1);
            // Details: label on the text column, value right-aligned.
            for (k, (label, value)) in details.iter().enumerate() {
                let rc = rect_wh(0, self.y_rows + k as i32 * self.row_h, self.width, self.row_h);
                let y = center_y(&rc, ln);
                s.text(label, col, y, Font::Normal, WHITE, TEXT2_ALPHA);
                let vw = s.text_width(value, Font::Normal);
                s.text(value, self.width - col - vw, y, Font::Normal, WHITE, TEXT_ALPHA);
            }
        }

        hline(s, col, self.width - col, self.sep2);
        if let Some(rc) = self.hits.rect(ID_SETTINGS) {
            let h = row_plate(s, rc, ID_SETTINGS, hot, pressed);
            let label = crate::i18n::tr("Power & battery settings", "Impostazioni alimentazione e batteria");
            s.text(label, rc.left + m.px(12), center_y(&rc, ln), Font::Normal, WHITE, lift(TEXT_ALPHA, h));
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<u32> {
        self.hits.at(x, y)
    }

    fn click(&mut self, id: u32) -> Reply {
        if id == ID_SETTINGS {
            self.open_settings = true;
            return Reply::Close;
        }
        Reply::None
    }

    fn data(&mut self, ev: &DataEvent) -> Reply {
        match ev {
            DataEvent::Battery(status) if *status != self.status => {
                let n = rows(&self.status, self.mode).len();
                self.status = *status;
                if rows(&self.status, self.mode).len() == n {
                    Reply::Redraw
                } else {
                    Reply::Relayout
                }
            }
            _ => Reply::None,
        }
    }

    fn after_close(&mut self) {
        if self.open_settings {
            launch("ms-settings:batterysaver");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_by_state() {
        let t = |ac, flag, p| title(&BatteryStatus::from_raw(ac, flag, p, u32::MAX));
        assert_eq!(t(0, 1, 64), ("64%".to_string(), "On battery".to_string()));
        assert_eq!(t(1, 9, 80), ("80%".to_string(), "Charging".to_string()));
        assert_eq!(t(1, 1, 100), ("100%".to_string(), "Fully charged".to_string()));
        assert_eq!(t(1, 1, 90), ("90%".to_string(), "Plugged in".to_string()));
        assert_eq!(title(&BatteryStatus::NONE), ("No battery".to_string(), String::new()));
    }

    #[test]
    fn rows_follow_what_windows_reports() {
        let on = BatteryStatus::from_raw(0, 1, 64, 11520);
        assert_eq!(
            rows(&on, Some(PowerMode::Balanced)),
            [("Time remaining", "3 h 12 min".to_string()), ("Power plan", "Balanced".to_string())]
        );
        let charging = BatteryStatus::from_raw(1, 9, 64, 11520);
        assert_eq!(rows(&charging, Some(PowerMode::Balanced)), [("Power plan", "Balanced".to_string())]);
        assert!(rows(&on, None).iter().all(|(l, _)| *l != "Power plan")); // unknown plan: no row
        assert!(rows(&BatteryStatus::from_raw(0, 1, 64, u32::MAX), None).is_empty()); // nothing known: no rows
    }
}
