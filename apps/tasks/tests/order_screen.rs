//! TA-T2 (T2.2, T2.3) on the screen: the hold's timer, the click guard and its flash, new rows, through the kit Harness
//! with the clock faked. No window, no desktop.

use std::path::Path;
use std::sync::Arc;

use oled_sysmon::ProcKey;
use oled_ui::geom::PointL;
use oled_ui::harness::Harness;
use oled_ui::model::{CellOut, ListModel, RowKey, Selection, TableModel};
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::engine::frame::Frame;
use tasks::ui::rows::{proc_key, RowKind, COL_NAME};
use tasks::ui::scene::{self, Scene};
use tasks::ui::screen::{InjectFrame, TasksScreen};

/// The calm scene with every process quiet except two: row A steady at 6 %, row B at 1 % for three ticks and then 9 %.
fn scene_ab(ticks: u32) -> Scene {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes").join("t1_list_calm.json");
    let mut s = Scene::load(&path).unwrap_or_else(|e| panic!("{e}"));
    s.ticks = ticks;
    s.sort = Some(scene::SceneSort { col: scene::SceneCol::Cpu, desc: true });
    for (i, p) in s.procs.iter_mut().enumerate() {
        p.cpu = match i {
            1 => vec![6.0],
            2 => {
                let mut v = vec![1.0; 3];
                v.push(9.0);
                v
            }
            _ => vec![0.0],
        };
        p.io = vec![0.0];
    }
    s
}

struct Rig {
    h: Harness<TasksScreen>,
    frames: Vec<tasks::engine::frame::Frame>,
    next: usize,
}

fn rig(ticks: u32) -> Rig {
    rig_from(scene_ab(ticks))
}

fn rig_from(mut s: Scene) -> Rig {
    let frames = s.frames();
    s.ticks = 0;
    let h = scene::open(
        &s,
        PreviewLang::En,
        96,
        Arc::new(RecordingLauncher::default()),
        Arc::new(RecordingExecutor::default()),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    Rig { h, frames, next: 1 }
}

impl Rig {
    fn send_next(&mut self) {
        let f = self.frames.remove(self.next.min(self.frames.len() - 1));
        self.h.send(Box::new(InjectFrame(f)));
    }

    fn order(&self) -> Vec<u64> {
        let m = self.h.screen().table().model();
        (0..m.len()).map(|i| m.key(i).0).collect()
    }

    fn row_point(&self, row: usize) -> PointL {
        let r = self.h.screen().table().row_rect(row).unwrap_or_else(|| panic!("row {row} is not visible"));
        PointL { x: (r.l + r.r) / 2, y: (r.t + r.b) / 2 }
    }
}

#[test]
fn the_hold_ends_by_its_timer() {
    let mut r = rig(0);
    r.h.advance(100);
    let p = r.row_point(1);
    r.h.hover(p);
    assert!(r.h.screen().header().held(), "the pointer moved in the list");
    r.h.advance(3000);
    assert!(r.h.screen().header().held(), "3 s after the move, 600 ms to go");
    r.h.advance(700);
    assert!(!r.h.screen().header().held(), "released by the timer, with no other input");
    r.h.assert_named();
}

#[test]
fn guarded_click_dropped() {
    let mut r = rig(12);
    r.h.advance(100);
    let p = r.row_point(1);
    r.h.hover(p);
    let before = r.order();
    let mut moved = false;
    // One frame a second; the rows may move once the pointer has been still for 3.6 s.
    for step in 0..120 {
        r.h.advance(100);
        if step % 10 == 9 {
            r.send_next();
        }
        if r.order() != before {
            moved = true;
            break;
        }
    }
    assert!(moved, "the rows never moved under the pointer");
    assert!(r.h.screen().table().selection().is_empty());
    let calm = r.h.render();
    r.h.advance(100);
    r.h.click(p);
    assert!(r.h.screen().table().selection().is_empty(), "a click 100 ms after the move does not reach the table");
    let flash = r.h.render();
    let row = r.h.screen().table().row_rect(1).unwrap();
    let w = flash.w as usize;
    let differs = (row.t..row.b)
        .any(|y| (row.l..row.r).any(|x| flash.px[y as usize * w + x as usize] != calm.px[y as usize * w + x as usize]));
    assert!(differs, "the row under the pointer flashes");
    r.h.advance(400);
    r.h.click(p);
    assert!(!r.h.screen().table().selection().is_empty(), "300 ms or more after the move, the click goes through");
    r.h.assert_named();
}

fn key_of(s: &Scene, i: usize) -> (ProcKey, RowKey) {
    let k = ProcKey { pid: s.procs[i].pid, created: s.procs[i].created };
    (k, proc_key(k))
}

/// The scene's second frame: the same processes, minus `gone` (the sampler's own diff is not in a scene).
fn frame_without(s: &Scene, gone: &[usize], seq: u64) -> Frame {
    let mut s2 = s.clone();
    let ended: Vec<ProcKey> = gone.iter().map(|&i| key_of(s, i).0).collect();
    for &i in gone.iter().rev() {
        s2.procs.remove(i);
    }
    s2.ticks = 0;
    let mut f = s2.frames().remove(0);
    f.seq = seq;
    f.gone = ended;
    f
}

fn row_kind(r: &Rig, key: RowKey) -> Option<RowKind> {
    r.h.screen().table().model().rows.iter().find(|e| e.key == key).map(|e| e.kind)
}

#[test]
fn an_ended_row_stays_selected_and_ended_while_held_then_goes() {
    let s = scene_ab(0);
    let (pk, key) = key_of(&s, 5);
    let mut r = rig_from(s.clone());
    r.h.advance(100);
    let row = r.h.screen().table().model().row_of(key).expect("the row is there");
    let p = r.row_point(row);
    r.h.click(p);
    assert_eq!(*r.h.screen().table().selection(), Selection::Single(key));
    r.h.advance(100);
    r.h.send(Box::new(InjectFrame(frame_without(&s, &[5], 2))));
    assert_eq!(row_kind(&r, key), Some(RowKind::Ghost(pk)), "the ended row stays while the pointer holds the list");
    assert_eq!(*r.h.screen().table().selection(), Selection::Single(key), "and so does its selection");
    let mut cell = CellOut::default();
    r.h.screen().table().model().cell(row, COL_NAME, &mut cell);
    let note = cell.note.as_ref().expect("an ended row says so");
    assert_eq!(note.1, oled_ui::model::FgRole::Disabled);
    // No further frame: the timer alone releases it (3 s of pointer hold, then 600 ms).
    r.h.advance(4000);
    assert_eq!(row_kind(&r, key), None, "the ghost goes at the release, with no frame");
    assert!(r.h.screen().table().selection().is_empty(), "and the selection with it");
    r.h.assert_named();
}

#[test]
fn a_ghost_goes_when_the_release_is_reached_by_an_event_outside_the_table() {
    let s = scene_ab(0);
    let (_, key) = key_of(&s, 5);
    let mut r = rig_from(s.clone());
    r.h.advance(100);
    let row = r.h.screen().table().model().row_of(key).expect("the row is there");
    let p = r.row_point(row);
    r.h.hover(p);
    r.h.send(Box::new(InjectFrame(frame_without(&s, &[5], 2))));
    assert!(row_kind(&r, key).is_some());
    r.h.advance(3590);
    // A move over the toolbar, just before the deadline and after it.
    r.h.hover(PointL { x: 300, y: 12 });
    r.h.advance(30);
    assert!(row_kind(&r, key).is_some(), "the release: the ghost fades out where it was (T2.4)");
    r.h.advance(200);
    assert_eq!(row_kind(&r, key), None, "no frame came, the ghost is gone");
    r.h.assert_named();
}

#[test]
fn a_new_row_wears_its_mark_three_seconds_and_the_screen_goes_idle() {
    let s = scene_ab(0);
    let mut s_more = s.clone();
    let mut extra = s_more.procs[5].clone();
    extra.pid = 9001;
    extra.created += 1;
    extra.image = "fresh.exe".into();
    s_more.procs.push(extra.clone());
    let new_key = ProcKey { pid: extra.pid, created: extra.created };
    let mut r = rig_from(s);
    r.h.advance(100);
    let mut f = {
        s_more.ticks = 0;
        s_more.frames().remove(0)
    };
    f.seq = 2;
    f.added = vec![new_key];
    r.h.send(Box::new(InjectFrame(f)));
    let fresh =
        |r: &Rig| r.h.screen().table().model().rows.iter().find(|e| e.key == proc_key(new_key)).map(|e| e.out.fresh);
    assert_eq!(fresh(&r), Some(true));
    r.h.advance(3100);
    assert_eq!(fresh(&r), Some(false), "the mark ends with no further frame");
    assert!(r.h.is_idle(), "nothing is left armed");
    r.h.assert_named();
}

/// 30 processes in a descending row, one of them (the 22nd) rising to the top.
fn tall_scene(ticks: u32) -> Scene {
    let mut s = scene_ab(ticks);
    for (i, p) in s.procs.iter_mut().enumerate() {
        p.cpu = match i {
            22 => vec![18.0, 18.0, 18.0, 60.0],
            1..=30 => vec![40.0 - i as f32],
            _ => vec![0.0],
        };
    }
    s
}

#[test]
fn a_focused_row_that_jumps_does_not_fool_the_click_guard() {
    let s = tall_scene(12);
    let (_, key) = key_of(&s, 22);
    let mut r = rig_from(s);
    r.h.advance(100);
    let row = r.h.screen().table().model().row_of(key).expect("the row is there");
    // Scroll until the focused row and the one two below it are on screen.
    let t = r.h.screen().table();
    let mid = PointL { x: (t.viewport().l + t.viewport().r) / 2, y: (t.viewport().t + t.viewport().b) / 2 };
    for _ in 0..40 {
        let t = r.h.screen().table();
        if t.row_rect(row).is_some() && t.row_rect(row + 2).is_some() {
            break;
        }
        r.h.wheel(mid, -1.0);
        r.h.advance(100);
    }
    assert!(r.h.screen().table().row_rect(row + 2).is_some(), "the scroll did not bring the rows into view");
    r.h.advance(500);
    let at = r.row_point(row);
    r.h.click(at);
    assert_eq!(*r.h.screen().table().selection(), Selection::Single(key));
    let rest = r.row_point(row + 2);
    r.h.hover(rest);
    let before = r.order();
    let mut moved = false;
    for step in 0..150 {
        r.h.advance(100);
        if step % 10 == 9 {
            r.send_next();
        }
        if r.order() != before {
            moved = true;
            break;
        }
    }
    assert!(moved, "the focused row never rose");
    r.h.advance(100);
    r.h.click(rest);
    assert_eq!(
        *r.h.screen().table().selection(),
        Selection::Single(key),
        "the click 100 ms after the jump is dropped, whatever the scroll put under the pointer"
    );
    r.h.assert_named();
}

#[test]
fn picking_a_menu_item_ends_the_hold_like_closing_the_menu() {
    let mut r = rig(12);
    r.h.advance(100);
    r.h.key("F10");
    assert_eq!(r.h.menus().len(), 1, "the More menu is open");
    assert!(r.h.screen().header().held(), "an open menu holds the order");
    // The third item, "Always on top": the first two (Refresh speed, Density) are submenus.
    r.h.key("Down");
    r.h.key("Down");
    r.h.key("Down");
    r.h.key("Enter");
    r.h.advance(700);
    assert!(!r.h.screen().header().held(), "the pick ended the menu");
    let before = r.order();
    let mut moved = false;
    for step in 0..120 {
        r.h.advance(100);
        if step % 10 == 9 {
            r.send_next();
        }
        if r.order() != before {
            moved = true;
            break;
        }
    }
    assert!(moved, "the rows never moved again after the pick");
    r.h.assert_named();
}

#[test]
fn a_dropped_press_released_elsewhere_does_not_eat_the_next_click() {
    let mut r = rig(12);
    r.h.advance(100);
    let p = r.row_point(1);
    r.h.hover(p);
    let before = r.order();
    for step in 0..120 {
        r.h.advance(100);
        if step % 10 == 9 {
            r.send_next();
        }
        if r.order() != before {
            break;
        }
    }
    assert_ne!(r.order(), before, "the rows never moved under the pointer");
    r.h.advance(100);
    let more = r.h.screen().chrome().more_rect();
    let at = PointL { x: (more.l + more.r) / 2, y: (more.t + more.b) / 2 };
    // The press is dropped by the guard; its release is elsewhere, on the More button: no Click is made.
    r.h.drag(p, at);
    assert!(r.h.menus().is_empty());
    r.h.advance(400);
    r.h.click(at);
    assert_eq!(r.h.menus().len(), 1, "the More button works again");
    r.h.assert_named();
}

#[test]
fn losing_the_activation_while_ctrl_is_down_lets_the_order_go() {
    let mut r = rig(0);
    r.h.advance(100);
    r.h.key("Ctrl+A");
    assert!(r.h.screen().header().held(), "Ctrl is down");
    r.h.set_active(false);
    r.h.advance(700);
    assert!(!r.h.screen().header().held(), "the key-up went to another window");
    r.h.assert_named();
}
