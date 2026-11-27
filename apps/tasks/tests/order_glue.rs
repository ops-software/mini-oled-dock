//! TA-T2 (T2.1 to T2.3): the order that holds still, over rows built by hand (no window): per-scope order, the hold,
//! ghosts and new rows. The screen's own cases (the click guard, the timers) are in order_screen.rs.

use std::cell::Cell;

use oled_sysmon::ProcKey;
use oled_ui::model::{RowKey, RowOut};
use tasks::engine::frame::Frame;
use tasks::model::view::{ColumnId, Section};
use tasks::ui::order::{self, OrderGlue};
use tasks::ui::rows::{RowEntry, RowKind, RowVals};
use tasks::ui::view::View;

fn pk(n: u32) -> ProcKey {
    ProcKey { pid: n, created: 1 }
}

fn entry(key: u64, kind: RowKind, parent: Option<u64>, cpu: Option<f32>) -> RowEntry {
    RowEntry {
        key: RowKey(key),
        kind,
        section: None,
        parent: parent.map(RowKey),
        depth: 0,
        vals: RowVals { cpu, ..RowVals::default() },
        out: RowOut::default(),
        motion: Cell::new((0, 255)),
        expandable: false,
        expanded: false,
    }
}

/// A process row whose key is its pid.
fn proc(n: u32, parent: Option<u64>, cpu: f32) -> RowEntry {
    entry(u64::from(n), RowKind::Proc(pk(n)), parent, Some(cpu))
}

fn keys(rows: &[RowEntry]) -> Vec<u64> {
    rows.iter().map(|r| r.key.0).collect()
}

fn cpu_view() -> View {
    let mut v = View::default();
    v.pin_sort(ColumnId::Cpu, true);
    v
}

/// One tick: the rows in the order the builder gives them (by key), ordered by the glue.
fn tick(v: &View, g: &mut OrderGlue, mut rows: Vec<RowEntry>, now: u64) -> Vec<RowEntry> {
    rows.sort_by_key(|r| r.key.0);
    order::apply(v, g, &mut rows, now);
    rows
}

#[test]
fn no_jump_inside_margin() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    let two = |a: f32, b: f32| vec![proc(1, None, a), proc(2, None, b)];
    assert_eq!(keys(&tick(&v, &mut g, two(10.0, 9.9), 0)), [1, 2]);
    // 0.3 points apart: the row never passes, however long it stays there.
    for i in 1..=8 {
        assert_eq!(keys(&tick(&v, &mut g, two(10.0, 10.3), i * 2000)), [1, 2], "tick {i}");
    }
    // 2.0 points apart: two ticks, and 2 s since the last move.
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, two(10.0, 9.9), 0);
    assert_eq!(keys(&tick(&v, &mut g, two(10.0, 12.0), 500)), [1, 2], "one tick is not enough");
    assert_eq!(keys(&tick(&v, &mut g, two(10.0, 12.0), 1000)), [1, 2], "two ticks, but not 2 s since the last move");
    assert_eq!(keys(&tick(&v, &mut g, two(10.0, 12.0), 2000)), [2, 1]);
}

#[test]
fn full_sort_on_column_change() {
    let mut g = OrderGlue::default();
    let mut v = cpu_view();
    let mk = || {
        let mut a = proc(1, None, 10.0);
        let mut b = proc(2, None, 9.9);
        a.vals.mem = Some(100);
        b.vals.mem = Some(200);
        vec![a, b]
    };
    assert_eq!(keys(&tick(&v, &mut g, mk(), 0)), [1, 2]);
    // Memory is within the floor of 1 MiB: only a full sort puts row 2 first, at once.
    v.pin_sort(ColumnId::Memory, true);
    assert_eq!(keys(&tick(&v, &mut g, mk(), 10)), [2, 1]);
}

/// Every row sits after its parent, inside its parent's run.
fn assert_forest(rows: &[RowEntry]) {
    for (i, r) in rows.iter().enumerate() {
        let Some(p) = r.parent else { continue };
        let at = rows.iter().position(|x| x.key == p).unwrap_or_else(|| panic!("parent of {} missing", r.key.0));
        assert!(at < i, "row {} above its parent", r.key.0);
        for between in &rows[at + 1..i] {
            let mut up = between.parent;
            let mut inside = false;
            while let Some(u) = up {
                if u == p {
                    inside = true;
                    break;
                }
                up = rows.iter().find(|x| x.key == u).and_then(|x| x.parent);
            }
            assert!(inside, "row {} breaks the run of {}", between.key.0, p.0);
        }
    }
}

fn grouped(cpu: &[f32; 9]) -> Vec<RowEntry> {
    let mut rows = vec![
        entry(100, RowKind::Section(Section::Apps), None, None),
        entry(200, RowKind::Section(Section::Background), None, None),
        entry(300, RowKind::Section(Section::Windows), None, None),
    ];
    // Groups 11, 12 under Apps; 21, 22 under Background; 31 under Windows; members 111, 112, 211 under their groups.
    let spec: [(u64, u64); 9] =
        [(11, 100), (12, 100), (21, 200), (22, 200), (31, 300), (111, 11), (112, 11), (211, 21), (121, 12)];
    for (i, (key, parent)) in spec.iter().enumerate() {
        let kind = if *key < 100 {
            RowKind::Group(tasks::model::group::AppKey::Single(pk(*key as u32)))
        } else {
            RowKind::Proc(pk(*key as u32))
        };
        rows.push(entry(*key, kind, Some(*parent), Some(cpu[i])));
    }
    rows
}

/// The builder's shape: each section row, its groups, each group row and its members.
fn flatten(rows: &[RowEntry]) -> Vec<RowEntry> {
    let mut out = Vec::new();
    fn walk(rows: &[RowEntry], parent: Option<u64>, out: &mut Vec<RowEntry>) {
        for r in rows.iter().filter(|r| r.parent.map(|p| p.0) == parent) {
            out.push(r.clone());
            walk(rows, Some(r.key.0), out);
        }
    }
    walk(rows, None, &mut out);
    out
}

#[test]
fn sections_never_mix() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut now = 0;
    for _ in 0..60 {
        let mut cpu = [0.0f32; 9];
        for c in &mut cpu {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *c = (x % 400) as f32 / 4.0;
        }
        let mut rows = flatten(&grouped(&cpu));
        order::apply(&v, &mut g, &mut rows, now);
        now += 2000;
        assert_forest(&rows);
        for (i, k) in [(0usize, 100u64), (1, 200), (2, 300)] {
            // The anchors keep their order; each is followed by its own groups.
            let at = rows.iter().position(|r| r.key.0 == k).unwrap();
            assert!(at >= i);
        }
        let anchors: Vec<u64> = rows.iter().filter(|r| r.is_anchor()).map(|r| r.key.0).collect();
        assert_eq!(anchors, [100, 200, 300]);
        assert_eq!(rows.len(), 12, "no row lost or made");
    }
}

#[test]
fn tree_siblings_only() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    // Two roots (1: 50%, 2: 5%); 1 has children 11, 12, 13; 2 has 21, 22.
    let make = |c: [f32; 5]| {
        let mut rows = vec![
            proc(1, None, 50.0),
            proc(11, Some(1), c[0]),
            proc(12, Some(1), c[1]),
            proc(13, Some(1), c[2]),
            proc(2, None, 5.0),
            proc(21, Some(2), c[3]),
            proc(22, Some(2), c[4]),
        ];
        // The builder's order: by value inside each parent is not guaranteed, so shuffle it by key.
        rows.sort_by_key(|r| r.key.0);
        flatten(&rows)
    };
    let mut rows = make([30.0, 20.0, 10.0, 3.0, 1.0]);
    order::apply(&v, &mut g, &mut rows, 0);
    assert_eq!(keys(&rows), [1, 11, 12, 13, 2, 21, 22]);
    // Child 13 grows to the top of its siblings and 22 overtakes 21: two ticks later they move, nothing else does.
    for now in [2000, 4000] {
        rows = make([30.0, 20.0, 60.0, 1.0, 9.0]);
        order::apply(&v, &mut g, &mut rows, now);
    }
    assert_eq!(keys(&rows), [1, 13, 11, 12, 2, 22, 21]);
    assert_forest(&rows);
}

#[test]
fn a_row_without_a_value_sorts_last_and_anchors_stay() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    let mut rows = vec![
        entry(1, RowKind::Section(Section::Apps), None, None),
        entry(2, RowKind::Proc(pk(2)), Some(1), None),
        proc(3, Some(1), 1.0),
        entry(4, RowKind::Hardware, None, Some(9.0)),
        proc(5, Some(4), 2.0),
    ];
    order::apply(&v, &mut g, &mut rows, 0);
    assert_eq!(keys(&rows), [1, 3, 2, 4, 5]);
}

// ---- T2.2: the hold

fn rows3() -> Vec<RowEntry> {
    vec![proc(1, None, 30.0), proc(2, None, 20.0), proc(3, None, 10.0)]
}

#[test]
fn hold_by_pointer() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    for now in [0, 1000, 2000] {
        order::note_pointer_move(&mut g, now);
        tick(&v, &mut g, rows3(), now);
        assert!(order::is_held(&g, now), "moving at {now}");
    }
    // Still for 3 s, then 600 ms more: released.
    tick(&v, &mut g, rows3(), 5599);
    assert!(order::is_held(&g, 5599));
    tick(&v, &mut g, rows3(), 5600);
    assert!(!order::is_held(&g, 5600));
}

#[test]
fn list_keys_hold_three_seconds() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    order::note_list_key(&mut g, 100);
    tick(&v, &mut g, rows3(), 3000);
    assert!(order::is_held(&g, 3000));
    tick(&v, &mut g, rows3(), 3700);
    assert!(!order::is_held(&g, 3700));
}

#[test]
fn menu_open_holds() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    order::note_menu_open(&mut g);
    tick(&v, &mut g, rows3(), 60_000);
    assert!(order::is_held(&g, 60_000));
    order::on_menu_closed(&mut g);
    tick(&v, &mut g, rows3(), 60_100);
    assert!(order::is_held(&g, 60_100), "600 ms after the menu closed");
    tick(&v, &mut g, rows3(), 60_700);
    assert!(!order::is_held(&g, 60_700));
}

#[test]
fn busy_row_holds() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    order::set_row_busy(&mut g, true);
    tick(&v, &mut g, rows3(), 10_000);
    assert!(order::is_held(&g, 10_000));
    order::set_row_busy(&mut g, false);
    tick(&v, &mut g, rows3(), 10_700);
    assert!(!order::is_held(&g, 10_700));
}

#[test]
fn held_rows_do_not_move() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, rows3(), 0);
    order::note_menu_open(&mut g);
    let flipped = || vec![proc(1, None, 1.0), proc(2, None, 20.0), proc(3, None, 90.0)];
    for now in [2000, 4000, 6000] {
        assert_eq!(keys(&tick(&v, &mut g, flipped(), now)), [1, 2, 3], "held at {now}");
    }
    order::on_menu_closed(&mut g);
    // The keys were learned while held: the move is immediate once released.
    assert_eq!(keys(&tick(&v, &mut g, flipped(), 6300)), [1, 2, 3], "still inside the 600 ms");
    assert_eq!(keys(&tick(&v, &mut g, flipped(), 6700)), [3, 2, 1]);
}

// ---- T2.3: ghosts and new rows

fn frame_with(added: &[ProcKey], gone: &[ProcKey]) -> Frame {
    Frame { added: added.to_vec(), gone: gone.to_vec(), ..Frame::default() }
}

#[test]
fn ghost_stays_while_held_and_goes_at_release() {
    let (mut v, mut g) = (cpu_view(), OrderGlue::default());
    assert_eq!(keys(&tick(&v, &mut g, rows3(), 0)), [1, 2, 3]);
    order::note_list_key(&mut g, 100);
    // Process 2 ends.
    v.push_frame(frame_with(&[], &[pk(2)]));
    let rows = tick(&v, &mut g, vec![proc(1, None, 30.0), proc(3, None, 10.0)], 200);
    assert_eq!(keys(&rows), [1, 2, 3]);
    assert_eq!(rows[1].kind, RowKind::Ghost(pk(2)));
    assert!(rows[1].out.ghost && rows[1].out.disabled);
    assert_eq!(rows[1].vals.cpu, Some(20.0), "the values are frozen");
    assert_eq!(order::ghost_names(&g)[0].0, pk(2));
    // The ghost stays through the next frames while the hold lasts, then goes.
    v.push_frame(frame_with(&[], &[]));
    assert_eq!(keys(&tick(&v, &mut g, vec![proc(1, None, 30.0), proc(3, None, 10.0)], 3000)), [1, 2, 3]);
    // The release: the ghost fades out where it was (T2.4), and the next pass drops it.
    let rows = tick(&v, &mut g, vec![proc(1, None, 30.0), proc(3, None, 10.0)], 3700);
    assert_eq!(keys(&rows), [1, 2, 3]);
    assert!(order::ghost_names(&g).is_empty());
    let rows = tick(&v, &mut g, vec![proc(1, None, 30.0), proc(3, None, 10.0)], 3850);
    assert_eq!(keys(&rows), [1, 3]);
}

#[test]
fn an_ended_row_that_is_not_held_just_goes() {
    let (mut v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, rows3(), 0);
    v.push_frame(frame_with(&[], &[pk(2)]));
    assert_eq!(keys(&tick(&v, &mut g, vec![proc(1, None, 30.0), proc(3, None, 10.0)], 100)), [1, 3]);
}

#[test]
fn fresh_for_3_s_at_the_end() {
    let (mut v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, rows3(), 0);
    v.push_frame(frame_with(&[pk(9)], &[]));
    let mut four = rows3();
    four.push(proc(9, None, 99.0));
    let rows = tick(&v, &mut g, four.clone(), 100);
    assert_eq!(keys(&rows), [1, 2, 3, 9], "a new row goes to the end of its scope");
    assert!(rows[3].out.fresh && !rows[0].out.fresh);
    let fresh = |rows: &[RowEntry]| rows.iter().find(|r| r.key.0 == 9).map(|r| r.out.fresh);
    assert_eq!(fresh(&tick(&v, &mut g, four.clone(), 3099)), Some(true));
    assert_eq!(fresh(&tick(&v, &mut g, four, 3100)), Some(false), "the mark ends 3 s after the row came");
}

#[test]
fn the_first_rows_are_not_new() {
    let mut v = cpu_view();
    let mut g = OrderGlue::default();
    v.push_frame(frame_with(&[pk(1), pk(2), pk(3)], &[]));
    assert!(tick(&v, &mut g, rows3(), 0).iter().all(|r| !r.out.fresh));
}

#[test]
fn selected_ghost_keeps_its_key() {
    let (mut v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, rows3(), 0);
    order::note_menu_open(&mut g);
    v.push_frame(frame_with(&[], &[pk(3)]));
    let rows = tick(&v, &mut g, vec![proc(1, None, 30.0), proc(2, None, 20.0)], 100);
    // The selection is kept by key in the table: the ghost has the key of the row that was selected.
    assert_eq!((rows[2].key, rows[2].kind), (RowKey(3), RowKind::Ghost(pk(3))));
}

// ---- fix round: the reviewer's cases

#[test]
fn a_pick_ends_the_menu_like_a_close() {
    // The screen sends on_menu_closed for a Command and a ListPicked as well; here the glue after it.
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    tick(&v, &mut g, rows3(), 0);
    order::note_menu_open(&mut g);
    tick(&v, &mut g, rows3(), 100);
    assert!(order::is_held(&g, 100));
    order::on_menu_closed(&mut g);
    tick(&v, &mut g, rows3(), 800);
    assert!(!order::is_held(&g, 800), "600 ms after the pick");
}

#[test]
fn a_header_click_while_held_sorts_fully() {
    let mut g = OrderGlue::default();
    let mut v = cpu_view();
    let mk = || {
        let mut a = proc(1, None, 10.0);
        let mut b = proc(2, None, 9.9);
        a.vals.mem = Some(100);
        b.vals.mem = Some(200);
        vec![a, b]
    };
    tick(&v, &mut g, mk(), 0);
    order::note_list_key(&mut g, 10);
    v.pin_sort(ColumnId::Memory, true);
    assert_eq!(keys(&tick(&v, &mut g, mk(), 20)), [2, 1], "held, and still a full sort at once");
    assert!(order::is_held(&g, 20));
}

#[test]
fn name_sort_places_a_new_row_exactly_after_the_release() {
    let (mut v, mut g) = (View::default(), OrderGlue::default());
    v.pin_sort(ColumnId::Name, false);
    // The builder gives the rows in name order: here the key order is the name order.
    let four = |extra: bool| {
        let mut r = vec![proc(10, None, 1.0), proc(20, None, 1.0), proc(30, None, 1.0), proc(40, None, 1.0)];
        if extra {
            r.push(proc(25, None, 1.0));
        }
        r
    };
    assert_eq!(keys(&tick(&v, &mut g, four(false), 0)), [10, 20, 30, 40]);
    order::note_list_key(&mut g, 100);
    assert_eq!(keys(&tick(&v, &mut g, four(true), 200)), [10, 20, 30, 40, 25], "held: appended");
    assert_eq!(keys(&tick(&v, &mut g, four(true), 3800)), [10, 20, 25, 30, 40], "released: its alphabetical place");
}

#[test]
fn an_ended_group_keeps_its_place_while_held() {
    let (mut v, mut g) = (cpu_view(), OrderGlue::default());
    let all = flatten(&grouped(&[10.0, 9.0, 8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0]));
    let mut rows = all.clone();
    order::apply(&v, &mut g, &mut rows, 0);
    let before = keys(&rows);
    order::note_list_key(&mut g, 100);
    // Group 11 ends with both members: the builder no longer makes the group row nor its members.
    v.push_frame(frame_with(&[], &[pk(111), pk(112)]));
    let mut rest: Vec<RowEntry> = all.iter().filter(|r| ![11, 111, 112].contains(&r.key.0)).cloned().collect();
    order::apply(&v, &mut g, &mut rest, 200);
    assert_forest(&rest);
    assert_eq!(keys(&rest), before, "the group row and its members keep their indices");
    assert!(rest.iter().filter(|r| [11, 111, 112].contains(&r.key.0)).all(|r| r.out.ghost && r.out.disabled));
    // Still frozen on the next pass, then gone at the release.
    let mut rest: Vec<RowEntry> = all.iter().filter(|r| ![11, 111, 112].contains(&r.key.0)).cloned().collect();
    order::apply(&v, &mut g, &mut rest, 3000);
    assert_eq!(keys(&rest), before);
    let mut rest: Vec<RowEntry> = all.iter().filter(|r| ![11, 111, 112].contains(&r.key.0)).cloned().collect();
    order::apply(&v, &mut g, &mut rest, 3800);
    assert_eq!(rest.len(), before.len(), "the release: the ghosts fade where they were");
    let mut rest: Vec<RowEntry> = all.iter().filter(|r| ![11, 111, 112].contains(&r.key.0)).cloned().collect();
    order::apply(&v, &mut g, &mut rest, 3950);
    assert_eq!(rest.len(), before.len() - 3);
}

#[test]
fn losing_the_activation_lets_go_of_keys_and_buttons() {
    let (v, mut g) = (cpu_view(), OrderGlue::default());
    order::note_mods(&mut g, oled_ui::Mods::CTRL);
    tick(&v, &mut g, rows3(), 100);
    assert!(order::is_held(&g, 100));
    order::on_deactivate(&mut g);
    tick(&v, &mut g, rows3(), 800);
    assert!(!order::is_held(&g, 800));
}
