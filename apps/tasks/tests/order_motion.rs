use std::path::Path;
use std::sync::Arc;

use oled_sysmon::ProcKey;
use oled_ui::geom::PointL;
use oled_ui::harness::Harness;
use oled_ui::model::{ListModel, RowKey, RowMotion, TableModel};
use oled_ui::theme::{SysOverlay, ThemeSpec, Tokens};
use tasks::app::args::PreviewLang;
use tasks::app::exec::RecordingExecutor;
use tasks::app::launch::RecordingLauncher;
use tasks::engine::frame::Frame;
use tasks::ui::rows::proc_key;
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

fn key_of(s: &Scene, i: usize) -> (ProcKey, RowKey) {
    let k = ProcKey { pid: s.procs[i].pid, created: s.procs[i].created };
    (k, proc_key(k))
}

/// The scene's second frame: the same processes, minus `gone`.
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

/// The scene's frame with one more process (a new row), `seq` its number.
fn frame_with_new(s: &Scene, seq: u64) -> (Frame, RowKey) {
    let mut more = s.clone();
    let mut extra = more.procs[5].clone();
    extra.pid = 9001;
    extra.created += 1;
    extra.image = "fresh.exe".into();
    more.procs.push(extra.clone());
    let key = ProcKey { pid: extra.pid, created: extra.created };
    more.ticks = 0;
    let mut f = more.frames().remove(0);
    f.seq = seq;
    f.added = vec![key];
    (f, proc_key(key))
}

struct Rig {
    h: Harness<TasksScreen>,
    frames: Vec<Frame>,
    next: usize,
}

fn rig_with(mut s: Scene, tokens: Tokens) -> Rig {
    let frames = s.frames();
    s.ticks = 0;
    let h = scene::open_with(
        &s,
        PreviewLang::En,
        96,
        tokens,
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

    fn motions(&self) -> Vec<RowMotion> {
        let m = self.h.screen().table().model();
        (0..m.len()).map(|i| m.motion(i)).collect()
    }

    fn motion_of(&self, key: RowKey) -> Option<RowMotion> {
        let m = self.h.screen().table().model();
        m.row_of(key).map(|i| m.motion(i))
    }
}

fn oled() -> Tokens {
    Tokens::oled()
}

fn reduced() -> Tokens {
    Tokens::resolve(&ThemeSpec::oled(), &SysOverlay { reduced_motion: true, ..SysOverlay::none() })
}

/// Frames a second apart, no pointer in the list, until the order changes (the rows slide at that instant).
fn until_moved(r: &mut Rig) {
    // The first measured frame sorts every scope at once (no motion): let it go by.
    r.send_next();
    r.h.advance(1000);
    let before = r.order();
    for step in 0..120 {
        r.h.advance(100);
        if step % 10 == 9 {
            r.send_next();
        }
        if r.order() != before {
            return;
        }
    }
    panic!("the rows never moved");
}

#[test]
fn slide_reaches_zero() {
    let mut r = rig_with(scene_ab(12), oled());
    r.h.advance(100);
    until_moved(&mut r);
    r.h.render();
    let start = r.motions();
    assert!(start.iter().any(|m| m.dy != 0), "a row that changed index starts away from its place");
    assert!(start.iter().all(|m| m.opacity == 255), "a slide does not fade");
    r.h.advance(70);
    r.h.render();
    let mid = r.motions();
    assert!(mid.iter().any(|m| m.dy != 0 && m.dy.abs() < start.iter().map(|s| s.dy.abs()).max().unwrap_or(0)));
    r.h.advance(80);
    r.h.render();
    assert!(r.motions().iter().all(|m| *m == RowMotion::NONE), "dy is 0 after 140 ms");
    assert!(r.h.is_idle(), "nothing is left animating");
    r.h.assert_named();
}

#[test]
fn reduced_motion_snaps() {
    assert!(reduced().motion.reduced);
    let mut r = rig_with(scene_ab(12), reduced());
    r.h.advance(100);
    until_moved(&mut r);
    r.h.render();
    assert!(r.motions().iter().all(|m| *m == RowMotion::NONE), "the rows are at their places at once");
    r.h.advance(20);
    assert!(r.h.is_idle());
}

#[test]
fn motion_is_none_at_rest() {
    let mut r = rig_with(scene_ab(12), oled());
    r.h.advance(100);
    r.h.render();
    assert!(r.motions().iter().all(|m| *m == RowMotion::NONE), "the first frame has no motion");
    until_moved(&mut r);
    r.h.advance(400);
    r.h.render();
    assert!(r.motions().iter().all(|m| *m == RowMotion::NONE));
    assert!(r.h.is_idle());
}

#[test]
fn a_new_row_fades_in_over_160_ms() {
    let s = scene_ab(0);
    let (f, key) = frame_with_new(&s, 2);
    let mut r = rig_with(s, oled());
    r.h.advance(100);
    r.h.send(Box::new(InjectFrame(f)));
    r.h.render();
    assert_eq!(r.motion_of(key).map(|m| m.opacity), Some(0), "the new row starts transparent");
    r.h.advance(80);
    r.h.render();
    let mid = r.motion_of(key).map_or(0, |m| m.opacity);
    assert!(mid > 0 && mid < 255, "half way: {mid}");
    r.h.advance(100);
    r.h.render();
    assert_eq!(r.motion_of(key), Some(RowMotion::NONE));
}

#[test]
fn an_ended_row_fades_out_at_the_release_then_goes() {
    let s = scene_ab(0);
    let (_, key) = key_of(&s, 5);
    let mut r = rig_with(s.clone(), oled());
    r.h.advance(100);
    let row = r.h.screen().table().model().row_of(key).expect("the row is there");
    let p = r.row_point(row);
    r.h.hover(p);
    r.h.send(Box::new(InjectFrame(frame_without(&s, &[5], 2))));
    r.h.render();
    assert_eq!(r.motion_of(key), Some(RowMotion::NONE), "held: the ghost stays as it is");
    // The pointer stands still: the hold ends 3.6 s after the move.
    r.h.advance(3600 + 70);
    r.h.render();
    let mid = r.motion_of(key).expect("the ghost is still there").opacity;
    assert!(mid > 0 && mid < 255, "fading: {mid}");
    r.h.advance(100);
    assert_eq!(r.motion_of(key), None, "gone after the fade");
    r.h.render();
    assert!(r.motions().iter().any(|m| m.dy != 0), "the rows below close the gap");
    r.h.advance(200);
    r.h.render();
    assert!(r.motions().iter().all(|m| *m == RowMotion::NONE));
    assert!(r.h.is_idle());
    r.h.assert_named();
}

/// The pixels above the table's rows (the toolbar and the header).
fn above_rows(r: &mut Rig) -> Vec<u32> {
    let vp = r.h.screen().table().viewport();
    let img = r.h.render();
    img.px[..(vp.t as usize * img.w as usize)].to_vec()
}

#[test]
fn held_look_only_while_held() {
    let mut r = rig_with(scene_ab(0), oled());
    r.h.advance(100);
    let calm = above_rows(&mut r);
    let p = r.row_point(1);
    r.h.hover(p);
    assert!(r.h.screen().header().held());
    let held = above_rows(&mut r);
    assert_ne!(calm, held, "an outlined chevron and the pause glyph");
    assert!(held.iter().zip(&calm).filter(|(a, b)| a != b).count() > 8);
    r.h.advance(4000);
    assert!(!r.h.screen().header().held());
    assert_eq!(above_rows(&mut r), calm, "released: the filled chevron and no pause glyph");
    r.h.assert_named();
}
