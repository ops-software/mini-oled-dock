//! TA-T1b (T1.7 to T1.11): the process around the window, through the kit Harness. No window, no process, no sampler
//! thread; the only files are in a temp folder of this test.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use oled_ui::harness::Harness;
use oled_ui::host::{Placement, PlacementKey, PlacementStore};
use oled_ui::{
    CommandTable, Event, EventCx, LayoutCx, PaintCx, PointL, RectP, Reply, Screen, SizeL, SizeP, Tokens, UiLang,
};
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::app::paths::Paths;
use tasks::app::presence::PresenceMsg;
use tasks::app::state::{self, StateDirty, StateHandle};
use tasks::app::Deps;
use tasks::engine::cadence::period_ms;
use tasks::engine::frame::Frame;
use tasks::ui::screen::{InjectFrame, TasksScreen, TIMER_STATE};

const MAIN: PlacementKey = PlacementKey("main");

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tasks-app-t1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness_with(state: Option<StateHandle>) -> Harness<TasksScreen> {
    let mut deps =
        Deps::inert(UiLang::EN, Arc::new(RecordingLauncher::default()), Arc::new(RecordingExecutor::default()));
    if let Some(s) = state {
        deps.state = s;
    }
    Harness::new(TasksScreen::new(deps), SizeL { w: 640, h: 560 }, 96, Tokens::oled())
}

fn placement(dpi: u32) -> Placement {
    Placement { monitor: "M1".into(), rect: RectP::wh(10, 20, 640, 560), dpi, maximized: false }
}

/// The dpi of the one placement in state.json, or None while there is no file.
fn written_dpi(dir: &std::path::Path) -> Option<u64> {
    let bytes = std::fs::read(dir.join("state.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let map = v["placements"].as_object()?;
    assert!(map.len() <= 1);
    map.values().next().and_then(|p| p["dpi"].as_u64())
}

#[test]
fn first_frame_line_once() {
    let mut h = harness_with(None);
    let _ = h.render();
    let _ = h.render();
    assert_eq!(h.screen().deps().sampling.first_frame_lines(), 0, "the line follows the first event, not the paint");
    h.send(Box::new(InjectFrame(Frame::default())));
    h.send(Box::new(InjectFrame(Frame::default())));
    let _ = h.render();
    h.hover(PointL { x: 300, y: 300 });
    assert_eq!(h.screen().deps().sampling.first_frame_lines(), 1);
}

#[test]
fn snapshot_b_once() {
    let mut h = harness_with(None);
    let _ = h.render();
    h.send(Box::new(InjectFrame(Frame::default())));
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), 0);
    h.advance(249);
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), 0, "not before 250 ms");
    h.advance(2);
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), 1);
    h.send(Box::new(InjectFrame(Frame::default())));
    h.advance(5_000);
    assert_eq!(h.screen().deps().sampling.sample_now_calls(), 1, "exactly one");
}

#[test]
fn minimised_arms_nothing() {
    let mut h = harness_with(None);
    let _ = h.render();
    h.send(Box::new(InjectFrame(Frame::default())));
    h.hover(PointL { x: 300, y: 300 });
    h.send(Box::new(StateDirty));
    h.send(Box::new(InjectFrame(Frame::default())));
    h.send(Box::new(PresenceMsg { minimised: true, cloaked: false }));
    h.advance(6_100);
    assert!(h.is_idle(), "every one-shot has run and none is re-armed");
    let c = h.screen().deps().sampling.last_control().expect("a control was set");
    assert!(c.minimised_or_cloaked && !c.visible);
    assert_eq!(period_ms(&c, false), None, "a stopped sampler has no period");
    h.advance(60_000 - 6_100);
    assert!(h.is_idle(), "still nothing at 60 s");
}

#[test]
fn save_debounced_through_one_post() {
    let dir = temp("debounce");
    let state = state::load(&Paths { roaming: None, local: Some(dir.clone()) });
    let posts = Arc::new(AtomicU32::new(0));
    let n = Arc::clone(&posts);
    state.attach(Box::new(move || {
        n.fetch_add(1, Ordering::Relaxed);
        true
    }));
    let mut h = harness_with(Some(state.clone()));
    let _ = h.render();
    let mut store = state.clone();
    store.save(MAIN, &placement(96));
    h.advance(400);
    store.save(MAIN, &placement(120));
    h.advance(400);
    store.save(MAIN, &placement(144));
    assert_eq!(posts.load(Ordering::Relaxed), 1, "three saves in one burst, one post");
    // The post reaches the screen as the data message; the write follows 2 s after it.
    h.send(Box::new(StateDirty));
    h.advance(1_999);
    assert_eq!(written_dpi(&dir), None, "not before 2 s");
    h.advance(2);
    assert_eq!(written_dpi(&dir), Some(144), "one write with the last placement");
    // A save after the write starts a new burst.
    store.save(MAIN, &placement(192));
    assert_eq!(posts.load(Ordering::Relaxed), 2);
    drop(h); // the screen's Drop flushes
    assert_eq!(written_dpi(&dir), Some(192));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A screen whose every key press is a change made by the screen itself.
struct Probe {
    state: StateHandle,
    cmds: CommandTable,
}

impl Screen for Probe {
    fn layout(&mut self, _: &mut LayoutCx, avail: SizeP) -> SizeP {
        avail
    }
    fn paint(&self, _: &mut PaintCx) {}
    fn event(&mut self, cx: &mut EventCx, ev: &Event) -> Reply {
        match ev {
            Event::Key { down: true, .. } => state::note_change(cx, &self.state),
            Event::Timer(id) if *id == TIMER_STATE => state::on_timer(&self.state),
            _ => {}
        }
        Reply::None
    }
    fn commands(&self) -> &CommandTable {
        &self.cmds
    }
}

#[test]
fn note_change_arms_once() {
    let dir = temp("note");
    let state = state::load(&Paths { roaming: None, local: Some(dir.clone()) });
    let mut h = Harness::new(
        Probe { state: state.clone(), cmds: CommandTable::new() },
        SizeL { w: 300, h: 200 },
        96,
        Tokens::oled(),
    );
    let _ = h.render();
    h.key("F7");
    h.advance(100);
    h.key("F8");
    assert!(!h.is_idle(), "the one-shot is armed");
    h.advance(1_950); // 2,050 ms since the first change; a re-armed timer would fire at 2,100
    assert!(dir.join("state.json").exists(), "the second change did not push the write back");
    assert!(h.is_idle(), "nothing is left armed after the write");
    let _ = std::fs::remove_dir_all(&dir);
}
