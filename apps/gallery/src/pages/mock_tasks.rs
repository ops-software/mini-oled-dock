//! The mock_tasks page (K2a.6): a tasks-like screen over 300 fake processes, refreshed by one message per simulated
//! second: the first integration consumer of the table and the rehearsal of the tasks app's window.

use super::table::{columns, Demo, Row, Tables};
use super::{s, Page};
use oled_ui::cx::EventCx;
use oled_ui::event::Reply;
use oled_ui::geom::SizeL;
use oled_ui::host::AppMsg;
use oled_ui::model::{Badge, RowMotion, SelectionMode, TableStyle};
use oled_ui::node::WidgetId;
use oled_ui::screen::Screen;
use oled_ui::strings::UiLang;
use oled_ui::widgets::VirtualTable;

/// The refresh message: one per simulated second.
pub struct Tick;

/// Processes on the page.
pub const PROCESSES: usize = 300;

/// The fake processes: an Apps section, a Background section, children under every tenth app.
fn processes() -> Vec<Row> {
    let mut rows = Vec::with_capacity(PROCESSES + 2);
    rows.push(Row { key: 10_000, name: "Apps".into(), section: true, ..Row::default() });
    for i in 0..PROCESSES as u64 {
        if i == 40 {
            rows.push(Row { key: 10_001, name: "Background".into(), section: true, ..Row::default() });
        }
        let child = i % 10 == 5;
        rows.push(Row {
            key: i,
            name: format!("proc-{i}"),
            size: (i * 7919 % 1500 + 4) * 1024 * 1024 / 4,
            cpu: (i * 31 % 100) as f32 / 4.0,
            depth: u16::from(child),
            expandable: i % 10 == 4,
            expanded: i % 10 == 4,
            heat: ((i * 31 % 100) / 25) as u8,
            action: true,
            icon: Some((i % 3) as u32),
            badge: (i % 37 == 0).then_some(Badge::Shield),
            ..Row::default()
        });
    }
    rows
}

/// The page's screen: the shared table screen that also takes the one-second refresh.
struct MockTasks(Tables);

impl Screen for MockTasks {
    fn layout(&mut self, cx: &mut oled_ui::cx::LayoutCx, avail: oled_ui::geom::SizeP) -> oled_ui::geom::SizeP {
        self.0.layout(cx, avail)
    }

    fn paint(&self, cx: &mut oled_ui::cx::PaintCx) {
        self.0.paint(cx)
    }

    fn event(&mut self, cx: &mut EventCx, ev: &oled_ui::event::Event) -> Reply {
        self.0.event(cx, ev)
    }

    fn data(&mut self, cx: &mut EventCx, msg: AppMsg) -> Reply {
        if msg.downcast_ref::<Tick>().is_none() {
            return self.0.data(cx, msg);
        }
        for (_, t) in &mut self.0.tables {
            let tick = t.model().generation + 1;
            let m = t.model_mut();
            m.generation = tick;
            for r in m.rows.iter_mut().filter(|r| !r.section) {
                let step = ((r.key + tick) * 2654435761 % 7) as f32 - 3.0;
                r.cpu = (r.cpu + step * 0.25).clamp(0.0, 99.0);
                r.heat = (r.cpu / 25.0) as u8;
            }
            t.model_changed(cx, true);
        }
        Reply::None
    }

    fn commands(&self) -> &oled_ui::command::CommandTable {
        self.0.commands()
    }
}

/// The motion the page's frame shows: row 4 slid in by 6 px at half opacity.
const FRAME_MOTION: Option<(usize, RowMotion)> = Some((4, RowMotion { dy: 6, opacity: 128 }));

pub fn page(lang: UiLang) -> Option<Page> {
    Some(build(lang, FRAME_MOTION))
}

fn build(lang: UiLang, motion: Option<(usize, RowMotion)>) -> Page {
    let mut cols = columns(lang);
    cols.truncate(3);
    let mut demo = Demo::new(processes());
    demo.motion = motion;
    let mut t = VirtualTable::new(WidgetId(1), demo, cols, SelectionMode::Multi, TableStyle::TASKS);
    t.set_label(s(lang, "Processes", "Processi"));
    let screen = MockTasks(Tables::new(vec![((8, 8, 624, 464), t)]));
    Page { name: "mock_tasks", size: SizeL { w: 640, h: 480 }, states: 2, screen: Box::new(screen) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_ui::harness::Harness;
    use oled_ui::theme::Tokens;

    #[test]
    fn mock_tasks_is_idle_between_ticks() {
        let p = page(UiLang::EN).unwrap();
        let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
        let first = h.render();
        assert!(h.is_idle(), "nothing runs after the first frame");
        h.advance(900);
        assert!(h.is_idle(), "no timer and no animation between two ticks");
        let calm = h.render();
        assert_eq!(first.px, calm.px, "an idle second paints the same pixels");
        h.send(Box::new(Tick));
        let moved = h.render();
        assert_ne!(first.px, moved.px, "a tick refreshes the figures");
        h.advance(900);
        assert!(h.is_idle());
        // 300 processes, two sections.
        assert!(h.a11y_dump().contains("of 302"));
    }

    #[test]
    fn the_frame_shows_one_row_in_motion() {
        let render = |motion| {
            let p = build(UiLang::EN, motion);
            let mut h = Harness::new(p.screen, p.size, 96, Tokens::oled());
            let img = h.render();
            assert!(h.a11y_dump().contains("of 302"), "the rows are the same");
            img
        };
        let still = render(None);
        let moved = render(Some((4, RowMotion { dy: 6, opacity: 128 })));
        assert_ne!(still.px, moved.px, "the row in motion paints differently");
        assert_eq!(page(UiLang::EN).unwrap().states, 2, "the page's frame is the moved state");
        let shown = render(FRAME_MOTION);
        assert_eq!(shown.px, moved.px, "the page's own frame shows the moved row");
    }
}
