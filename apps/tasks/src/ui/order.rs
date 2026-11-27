//! Rows that hold still (SPEC-tasks 5.2.7 through the model's order rules): per-scope order, the hold under the
//! pointer and after a list key, the click guard, ghosts and new rows. TA-T2 (T2.1-T2.3); the release motion is
//! TA-T2M's (T2.4).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use oled_sysmon::ProcKey;
use oled_ui::model::{ListModel, RowKey, TableOut};
use oled_ui::tokens::DUR_NORMAL;
use oled_ui::widgets::VirtualTable;
use oled_ui::{AnimId, Ease, Event, EventCx, Mods, NodeId, PaintCx, PointP, RectP, TimerId};

use super::more_menu::row_height;
use super::rows::{sort_value, RowEntry, RowKind, TasksTable};
use super::screen::{TABLE, TIMER_FRESH, TIMER_HOLD};
use super::view::View;
use crate::app::options::Density;
use crate::model::order::{
    click_dropped, full_sort, hold, hold_deadline, reorder, HoldInputs, HoldState, OrderState, RowId, SortSpec,
};
use crate::model::view::{ColumnId, ViewMode};

/// How long a new row wears its mark.
pub const FRESH_MS: u64 = 3000;

/// The animation channel of the click guard's flash.
const FLASH_CHANNEL: u8 = 200;

/// The release motion's channels on the table node (T2.4): the slide (1 to 0), the new rows' fade-in (0 to 1) and the
/// ghosts' fade-out (1 to 0).
const SLIDE_CHANNEL: u8 = 1;
const BORN_CHANNEL: u8 = 2;
const FADE_CHANNEL: u8 = 3;

/// The farthest a row paints from its place while it slides, logical px.
const SLIDE_MAX: i32 = 4000;

/// One scope's order (the children of one parent) and the pass that last used it.
#[derive(Default)]
struct Scope {
    state: OrderState,
    gen: u64,
}

/// The order state between ticks.
#[derive(Default)]
pub struct OrderGlue {
    /// A small id per row key, what the model's order works on.
    ids: HashMap<RowKey, RowId>,
    next_id: RowId,
    /// One order per parent row (None: the top level, and all of List mode).
    scopes: HashMap<Option<RowKey>, Scope>,
    gen: u64,
    /// Some row of the last pass had a value to sort by.
    had_values: bool,
    /// The column, direction and mode of the last pass; a change sorts every scope afresh.
    last: Option<(ColumnId, bool, ViewMode)>,
    query_changed: bool,
    inputs: HoldInputs,
    hold: HoldState,
    /// The rows of the last pass, in order (ghosts included).
    shown: Vec<RowEntry>,
    /// The labels of the ghost rows.
    ghost_names: Vec<(ProcKey, String)>,
    /// New rows and when their mark ends.
    fresh_until: HashMap<RowKey, u64>,
    /// When the order or the row set last changed.
    order_changed_ms: Option<u64>,
    /// Where the pointer last moved inside the table, and the row key it had under it then.
    pointer_pos: Option<PointP>,
    pointer_key: Option<RowKey>,
    /// The time of the last pipeline pass (apply).
    applied_at: Option<u64>,
    /// The row the click guard flashes.
    flash: Option<RowKey>,
    /// A press was dropped by the click guard: the rest of its gesture is dropped too.
    swallowing: bool,
    /// The deadlines the two timers are armed for.
    hold_armed: Option<u64>,
    fresh_armed: Option<u64>,
    /// T2.4. The last pass's rows that changed index: how many rows each one is from where it was.
    moves: HashMap<RowKey, i32>,
    /// The rows that are new in the last pass, and the ghosts that fade out in it.
    born: HashSet<RowKey>,
    fading: HashSet<RowKey>,
    /// When the fading ghosts are removed.
    fade_until: Option<u64>,
    /// What sync starts for the last pass: the slide, the fade-in, the fade-out.
    start: [bool; 3],
    /// The last paint left every row of the maps at rest (nothing more to write until the next pass).
    settled: Cell<bool>,
}

impl OrderGlue {
    /// hold() at `now`; true while held.
    fn evaluate(&mut self, now: u64) -> bool {
        hold(&self.inputs, &mut self.hold, now)
    }
}

/// Whether a pointer event reaches the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerGate {
    /// A click within the guard after a move under the pointer: consumed, never reaches the table.
    Swallow,
    Pass,
}

/// A numeric column sorts by its displayed value; any other keeps the order the builder gave (a rank).
fn by_value(c: ColumnId) -> bool {
    matches!(c, ColumnId::Cpu | ColumnId::Memory | ColumnId::Io | ColumnId::Pid)
}

/// The metrics that fluctuate (the hysteresis of SPEC 5.2.7 covers them only); Name and PID sort exactly.
fn hysteretic(c: ColumnId) -> bool {
    matches!(c, ColumnId::Cpu | ColumnId::Memory | ColumnId::Io)
}

/// One pass over the rows as a forest: the children of every parent are ordered among themselves (anchors keep their
/// places), each child followed by its own subtree.
struct Pass<'a> {
    rows: &'a [RowEntry],
    ids: &'a HashMap<RowKey, RowId>,
    scopes: &'a mut HashMap<Option<RowKey>, Scope>,
    children: HashMap<Option<RowKey>, Vec<usize>>,
    column: ColumnId,
    spec: SortSpec,
    reset: bool,
    held: bool,
    now: u64,
    gen: u64,
    out: Vec<usize>,
}

impl Pass<'_> {
    fn emit(&mut self, parent: Option<RowKey>) {
        let Some(kids) = self.children.remove(&parent) else { return };
        let movable: Vec<usize> = kids.iter().copied().filter(|&i| !self.rows[i].is_anchor()).collect();
        let mut ordered = self.scope_order(parent, &movable).into_iter();
        for &slot in &kids {
            let i = if self.rows[slot].is_anchor() { slot } else { ordered.next().unwrap_or(slot) };
            self.out.push(i);
            self.emit(Some(self.rows[i].key));
        }
    }

    /// The movable children of one parent, in their new order.
    fn scope_order(&mut self, parent: Option<RowKey>, movable: &[usize]) -> Vec<usize> {
        let ids: Vec<RowId> = movable.iter().map(|&i| self.ids.get(&self.rows[i].key).copied().unwrap_or(0)).collect();
        let keys: Vec<f32> = movable
            .iter()
            .enumerate()
            .map(|(rank, &i)| {
                if by_value(self.column) {
                    sort_value(&self.rows[i], self.column).map_or(f32::NAN, |x| x as f32)
                } else {
                    rank as f32
                }
            })
            .collect();
        let sc = self.scopes.entry(parent).or_default();
        sc.gen = self.gen;
        if self.reset || sc.state.order.is_empty() {
            full_sort(&ids, &keys, self.spec, &mut sc.state, self.now);
        } else if hysteretic(self.column) || self.held {
            reorder(&ids, &keys, self.spec, self.held, &mut sc.state, self.now);
        } else {
            // Name and PID have no margin to wait out: a new row takes its exact place at the release.
            full_sort(&ids, &keys, self.spec, &mut sc.state, self.now);
        }
        let mut by_id: Vec<(RowId, usize)> = ids.iter().copied().zip(movable.iter().copied()).collect();
        by_id.sort_unstable_by_key(|e| e.0);
        sc.state
            .order
            .iter()
            .filter_map(|id| by_id.binary_search_by_key(id, |e| e.0).ok().map(|p| by_id[p].1))
            .collect()
    }
}

/// T2.1, T2.3. The pipeline step: ghosts of ended rows while held, the order of every scope (a full sort when the
/// column, direction, mode or query changed, else the hysteretic pass; held, no row moves), then the new rows' mark.
#[allow(clippy::ptr_arg)] // the frozen signature: the order and the ghosts change the vector
pub fn apply(v: &View, g: &mut OrderGlue, rows: &mut Vec<RowEntry>, now: u64) {
    let held = g.evaluate(now);
    let sort = v.sort();
    let sig = (sort.column, sort.descending, v.mode());
    let first = g.last.is_none();
    // The first measured figures (the first frame has none) sort every scope at once, whatever the hold says.
    let has_values = by_value(sort.column) && rows.iter().any(|r| sort_value(r, sort.column).is_some());
    let measured = has_values && !std::mem::replace(&mut g.had_values, has_values);
    g.had_values = has_values;
    let reset = g.last != Some(sig) || std::mem::take(&mut g.query_changed) || measured;
    g.last = Some(sig);
    let shown_at: HashMap<RowKey, usize> = g.shown.iter().enumerate().map(|(i, r)| (r.key, i)).collect();

    // Ghosts: a displayed process that ended stays where it was until the hold ends, with the rows above it that
    // ended too (a group row whose members all ended) so that it keeps its place among its siblings.
    let cur: HashSet<RowKey> = rows.iter().map(|r| r.key).collect();
    let old_names = std::mem::take(&mut g.ghost_names);
    if held && !reset {
        let frozen = |s: &RowEntry, kind: RowKind| {
            let mut ghost = s.clone();
            ghost.kind = kind;
            ghost.out.ghost = true;
            ghost.out.disabled = true;
            ghost.out.fresh = false;
            ghost.out.busy = false;
            ghost
        };
        let mut pushed: HashSet<RowKey> = HashSet::new();
        for s in &g.shown {
            if cur.contains(&s.key) {
                continue;
            }
            let (kind, name) = match s.kind {
                RowKind::Proc(pk) if v.gone().contains(&pk) => {
                    let mut name = String::new();
                    if !v.prev_name(pk, &mut name) {
                        name = pk.pid.to_string();
                    }
                    (RowKind::Ghost(pk), Some((pk, name)))
                }
                RowKind::Ghost(pk) => {
                    let name = old_names.iter().find(|e| e.0 == pk).map(|e| e.1.clone()).unwrap_or_default();
                    (RowKind::Ghost(pk), Some((pk, name)))
                }
                // A row frozen above the ghosts by an earlier pass.
                RowKind::Group(_) | RowKind::Proc(_) if s.out.ghost => (s.kind, None),
                _ => continue,
            };
            rows.push(frozen(s, kind));
            pushed.insert(s.key);
            g.ghost_names.extend(name);
        }
        // Walk up from each ghost: a parent that is no longer there (a group row) is kept, frozen, in its place.
        let mut at = 0;
        while at < rows.len() {
            let mut up = rows[at].parent;
            while let Some(pk) = up {
                if cur.contains(&pk) || pushed.contains(&pk) {
                    break;
                }
                let Some(s) = g.shown.iter().find(|x| x.key == pk) else { break };
                let kind = match s.kind {
                    RowKind::Group(_) => s.kind,
                    RowKind::Proc(p) if v.gone().contains(&p) => RowKind::Ghost(p),
                    _ => break,
                };
                if let RowKind::Ghost(p) = kind {
                    let mut name = String::new();
                    if !v.prev_name(p, &mut name) {
                        name = p.pid.to_string();
                    }
                    g.ghost_names.push((p, name));
                }
                rows.push(frozen(s, kind));
                pushed.insert(pk);
                up = s.parent;
            }
            at += 1;
        }
    }

    // New rows: a process the newest frame added that was not on screen before (nothing is new on the first pass or
    // after a change of the order's basis).
    if !first && !reset {
        let added = v.added();
        if !added.is_empty() {
            for r in rows.iter() {
                if let RowKind::Proc(pk) = r.kind {
                    if !shown_at.contains_key(&r.key) && added.contains(&pk) {
                        g.fresh_until.entry(r.key).or_insert(now + FRESH_MS);
                    }
                }
            }
        }
    }
    g.fresh_until.retain(|_, until| *until > now);
    if !g.fresh_until.is_empty() {
        for r in rows.iter_mut() {
            r.out.fresh = g.fresh_until.contains_key(&r.key);
        }
    }

    // Ids, then the order.
    let all: HashSet<RowKey> = rows.iter().map(|r| r.key).collect();
    g.ids.retain(|k, _| all.contains(k));
    for r in rows.iter() {
        if !g.ids.contains_key(&r.key) {
            g.next_id = g.next_id.wrapping_add(1);
            g.ids.insert(r.key, g.next_id);
        }
    }
    g.gen += 1;
    let index: HashMap<RowKey, usize> = rows.iter().enumerate().map(|(i, r)| (r.key, i)).collect();
    let mut children: HashMap<Option<RowKey>, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        let parent = r.parent.filter(|p| index.contains_key(p));
        children.entry(parent).or_default().push(i);
    }
    let spec = if by_value(sort.column) {
        SortSpec::for_column(sort.column, sort.descending)
    } else {
        SortSpec { floor: 0.0, descending: false }
    };
    let mut pass = Pass {
        rows,
        ids: &g.ids,
        scopes: &mut g.scopes,
        children,
        column: sort.column,
        spec,
        reset,
        held,
        now,
        gen: g.gen,
        out: Vec::with_capacity(rows.len()),
    };
    pass.emit(None);
    let order = std::mem::take(&mut pass.out);
    let gen = g.gen;
    g.scopes.retain(|_, s| s.gen == gen);
    let mut seen = vec![false; rows.len()];
    let sound =
        order.len() == rows.len() && order.iter().all(|&i| i < seen.len() && !std::mem::replace(&mut seen[i], true));
    if sound {
        let mut taken: Vec<Option<RowEntry>> = rows.drain(..).map(Some).collect();
        rows.extend(order.iter().filter_map(|&i| taken[i].take()));
    }

    // T2.4: what moved, what is new, which ghosts fade out in place before they go.
    g.moves.clear();
    g.born.clear();
    g.start = [false; 3];
    if held || first || reset {
        g.fading.clear();
        g.fade_until = None;
    } else if g.fading.is_empty() {
        // The hold ended: the ghosts of the held pass stay where they were while they fade (the next pass drops them).
        let mut ghosts: Vec<(usize, RowEntry)> = g
            .shown
            .iter()
            .enumerate()
            .filter(|(_, s)| s.out.ghost && !cur.contains(&s.key))
            .map(|(i, s)| (i, s.clone()))
            .collect();
        ghosts.sort_by_key(|e| e.0);
        for (i, s) in ghosts {
            g.fading.insert(s.key);
            rows.insert(i.min(rows.len()), s);
        }
        if !g.fading.is_empty() {
            g.fade_until = Some(now + u64::from(DUR_NORMAL));
            g.start[2] = true;
        }
    } else {
        g.fading.clear();
        g.fade_until = None;
    }
    if !first && !reset {
        for (i, r) in rows.iter().enumerate() {
            match shown_at.get(&r.key) {
                Some(&old) if old != i => {
                    g.moves.insert(r.key, old as i32 - i as i32);
                }
                None if !r.out.ghost && !r.is_anchor() => {
                    g.born.insert(r.key);
                }
                _ => {}
            }
        }
        g.start[0] = !g.moves.is_empty();
        g.start[1] = !g.born.is_empty();
    }
    if g.start.iter().any(|s| *s) {
        g.settled.set(false);
    }

    // A change of the order or of the row set starts the click guard's window.
    if !first && (g.shown.len() != rows.len() || g.shown.iter().zip(rows.iter()).any(|(a, b)| a.key != b.key)) {
        g.order_changed_ms = Some(now);
    }
    g.applied_at = Some(now);
    g.shown.clear();
    g.shown.extend(rows.iter().cloned());
}

/// The labels of the ghost rows, for the view (the frame no longer has them).
pub fn ghost_names(g: &OrderGlue) -> &[(ProcKey, String)] {
    &g.ghost_names
}

/// The search text changed: the next pass sorts every scope afresh.
pub fn note_query(g: &mut OrderGlue) {
    g.query_changed = true;
}

/// Modifier keys are held while Ctrl or Shift is down (a selection in progress holds the order).
pub fn note_mods(g: &mut OrderGlue, mods: Mods) {
    g.inputs.modifier_down = mods.ctrl() || mods.shift();
}

/// The pointer moved inside the table's viewport.
pub fn note_pointer_move(g: &mut OrderGlue, now: u64) {
    g.inputs.pointer_moved_in_list_ms = Some(now);
}

/// A mouse button went up anywhere.
pub fn note_button_up(g: &mut OrderGlue) {
    g.inputs.button_down = false;
}

/// This app opened a menu (it closes with Event::MenuClosed).
pub fn note_menu_open(g: &mut OrderGlue) {
    g.inputs.menu_open = true;
}

/// A row is being acted on (the actions' busy set is not empty).
pub fn set_row_busy(g: &mut OrderGlue, busy: bool) {
    g.inputs.row_busy = busy;
}

/// The row the click guard flashes, if any.
pub fn flash_key(g: &OrderGlue) -> Option<RowKey> {
    g.flash
}

/// Evaluates the hold and arms the two one-shot timers (the end of every event and data call).
pub fn sync(cx: &mut EventCx, g: &mut OrderGlue, now: u64) {
    let was_held = g.hold.held;
    g.evaluate(now);
    let after = |d: u64| d.saturating_sub(now).clamp(1, u64::from(u32::MAX)) as u32;
    if was_held && !g.hold.held && g.applied_at != Some(now) {
        // The hold ended on an event that did not run the pipeline: the pipeline runs on the next tick of the clock.
        cx.set_timer(TIMER_HOLD, 1);
        g.hold_armed = Some(now + 1);
    } else {
        sync_hold(cx, g, after);
    }
    start_motion(cx, g);
    sync_fresh(cx, g, now, after);
}

fn sync_hold(cx: &mut EventCx, g: &mut OrderGlue, after: impl Fn(u64) -> u32) {
    match hold_deadline(&g.inputs, &g.hold) {
        Some(d) if g.hold_armed != Some(d) => {
            cx.set_timer(TIMER_HOLD, after(d));
            g.hold_armed = Some(d);
        }
        None if g.hold_armed.take().is_some() => cx.cancel_timer(TIMER_HOLD),
        _ => {}
    }
}

fn sync_fresh(cx: &mut EventCx, g: &mut OrderGlue, now: u64, after: impl Fn(u64) -> u32) {
    // The same timer ends the mark of a new row and the fade of the ghosts (the pipeline drops them on its pass).
    let fade =
        g.fade_until
            .filter(|_| !g.fading.is_empty())
            .map(|d| if cx.tokens().motion.dur_normal == 0 { now + 1 } else { d });
    match g.fresh_until.values().copied().filter(|u| *u > now).chain(fade).min() {
        Some(d) if g.fresh_armed != Some(d) => {
            cx.set_timer(TIMER_FRESH, after(d));
            g.fresh_armed = Some(d);
        }
        None if g.fresh_armed.take().is_some() => cx.cancel_timer(TIMER_FRESH),
        _ => {}
    }
}

fn anim_id(channel: u8) -> AnimId {
    AnimId { node: NodeId::new(TABLE, 0), channel }
}

/// T2.4. Starts the release motion the last pass asked for (the end of every event and data call). Under reduced motion
/// the durations are 0 and every tween jumps to its end.
fn start_motion(cx: &mut EventCx, g: &mut OrderGlue) {
    let start = std::mem::take(&mut g.start);
    let m = cx.tokens().motion;
    if start[0] {
        cx.animate(anim_id(SLIDE_CHANNEL), 1.0, 0, Ease::Linear);
        cx.animate(anim_id(SLIDE_CHANNEL), 0.0, m.dur_normal, Ease::OutCubic);
    }
    if start[1] {
        cx.animate(anim_id(BORN_CHANNEL), 0.0, 0, Ease::Linear);
        cx.animate(anim_id(BORN_CHANNEL), 1.0, m.dur_slow, Ease::OutCubic);
    }
    if start[2] {
        cx.animate(anim_id(FADE_CHANNEL), 1.0, 0, Ease::Linear);
        cx.animate(anim_id(FADE_CHANNEL), 0.0, m.dur_normal, Ease::In);
    }
}

/// Writes each moving, new or fading RowEntry.motion while a release slides or fades (before the table paints). T2.4.
pub fn on_paint_anim(g: &OrderGlue, cx: &mut PaintCx, model: &TasksTable, density: Density) {
    if g.settled.get() || (g.moves.is_empty() && g.born.is_empty() && g.fading.is_empty()) {
        return;
    }
    let slide = cx.anim(anim_id(SLIDE_CHANNEL)).unwrap_or(0.0).clamp(0.0, 1.0);
    let born = cx.anim(anim_id(BORN_CHANNEL)).unwrap_or(1.0).clamp(0.0, 1.0);
    let fade = cx.anim(anim_id(FADE_CHANNEL)).unwrap_or(1.0).clamp(0.0, 1.0);
    let row_h = row_height(density, cx.tokens().size.row_table);
    let alpha = |t: f32| (t * 255.0).round() as u8;
    for (key, rows) in &g.moves {
        if let Some(i) = model.row_of(*key) {
            let dy = (*rows as f32 * row_h as f32 * slide).round() as i32;
            model.rows[i].motion.set((dy.clamp(-SLIDE_MAX, SLIDE_MAX) as i16, 255));
        }
    }
    let fade_in = |keys: &HashSet<RowKey>, t: f32| {
        for key in keys {
            if let Some(i) = model.row_of(*key) {
                let cell = &model.rows[i].motion;
                cell.set((cell.get().0, alpha(t)));
            }
        }
    };
    fade_in(&g.born, born);
    fade_in(&g.fading, fade);
    // Nothing more to write once the slide and the fade-in ended and no ghost is on its way out.
    g.settled.set(slide <= 0.0 && born >= 1.0 && g.fading.is_empty());
}

fn flash_id() -> AnimId {
    AnimId { node: NodeId::new(TABLE, 0), channel: FLASH_CHANNEL }
}

/// The short flash of a row that moved under a dropped click.
pub fn paint_flash(g: &OrderGlue, cx: &mut PaintCx, table: &VirtualTable<TasksTable>) {
    let Some(key) = g.flash else { return };
    let Some(t) = cx.anim(flash_id()).filter(|t| *t > 0.0) else { return };
    let Some(rect) = table.model().row_of(key).and_then(|row| table.row_rect(row)) else { return };
    let vp = table.viewport();
    let ink = cx.tokens().color.fill_press.faded(t);
    cx.fill(RectP { l: vp.l, r: vp.r, ..rect }, 0, ink);
}

/// TIMER_HOLD, TIMER_FRESH: the screen runs the pipeline after this; the timer is spent and may be armed again.
pub fn on_timer(cx: &mut EventCx, g: &mut OrderGlue, id: TimerId) {
    let _ = cx;
    if id == TIMER_HOLD {
        g.hold_armed = None;
    } else if id == TIMER_FRESH {
        g.fresh_armed = None;
    }
}

/// F5: the next pass sorts afresh and no hold is left (the pointer's and the keys').
pub fn release(g: &mut OrderGlue) {
    g.query_changed = true;
    g.inputs.list_key_ms = None;
    g.inputs.pointer_moved_in_list_ms = None;
    g.hold = HoldState::default();
}

/// A navigation key went to the focused table: the order holds 3 s.
pub fn note_list_key(g: &mut OrderGlue, now: u64) {
    g.inputs.list_key_ms = Some(now);
}

/// The key of the row under a point.
fn key_at(table: &VirtualTable<TasksTable>, p: PointP) -> Option<RowKey> {
    row_at(table, p).and_then(|r| table.model().rows.get(r)).map(|r| r.key)
}

/// The row index under a point, among the visible rows.
fn row_at(table: &VirtualTable<TasksTable>, p: PointP) -> Option<usize> {
    table.visible_rows().find(|&row| table.row_rect(row).is_some_and(|r| r.contains(p)))
}

/// The rest of a dropped press (its up, its click, a drag) never reaches the table.
pub fn swallows(g: &mut OrderGlue, ev: &Event) -> bool {
    if !g.swallowing {
        return false;
    }
    match ev {
        // A new press ends the old gesture, whatever became of its release.
        Event::PointerDown { .. } => {
            g.swallowing = false;
            false
        }
        Event::Click { .. } => {
            g.swallowing = false;
            true
        }
        Event::PointerUp { .. } | Event::DragStart { .. } | Event::DragMove { .. } | Event::DragEnd { .. } => true,
        _ => false,
    }
}

/// A pointer move, down or up inside the table's viewport: the hold, and the click guard.
pub fn on_pointer(
    cx: &mut EventCx,
    g: &mut OrderGlue,
    ev: &Event,
    table: &VirtualTable<TasksTable>,
    now: u64,
) -> PointerGate {
    match ev {
        Event::PointerMove { pos, mods } => {
            note_mods(g, *mods);
            note_pointer_move(g, now);
            if g.pointer_pos != Some(*pos) {
                g.pointer_pos = Some(*pos);
                g.pointer_key = key_at(table, *pos);
            }
        }
        Event::PointerDown { pos, mods, .. } => {
            note_mods(g, *mods);
            g.inputs.button_down = true;
            let row = row_at(table, *pos);
            let key_now = row.and_then(|r| table.model().rows.get(r)).map(|r| r.key);
            if g.pointer_pos != Some(*pos) {
                // A press with no move before it: the user aimed at what is on screen now.
                g.pointer_pos = Some(*pos);
                g.pointer_key = key_now;
            }
            // Dropped: another row is under the pointer than the one it moved onto, and the list changed just now.
            g.swallowing = g.pointer_key != key_now && click_dropped(now, g.order_changed_ms);
            g.pointer_key = key_now;
            if g.swallowing {
                if let Some(row) = row {
                    g.flash = key_now;
                    cx.animate(flash_id(), 1.0, 0, Ease::Linear);
                    cx.animate(flash_id(), 0.0, DUR_NORMAL, Ease::Linear);
                    if let Some(r) = table.row_rect(row) {
                        let vp = table.viewport();
                        cx.damage(RectP { l: vp.l, r: vp.r, ..r });
                    }
                }
                return PointerGate::Swallow;
            }
        }
        Event::PointerUp { mods, .. } => {
            note_mods(g, *mods);
            g.inputs.button_down = false;
        }
        _ => {}
    }
    PointerGate::Pass
}

/// What the table said. Nothing in it holds the order by itself: whoever opens a menu (the More menu, the row's context
/// menu of T3.9) calls note_menu_open, so a menu that did not open cannot hold the order.
pub fn on_table_out(g: &mut OrderGlue, out: &TableOut, now: u64) {
    let _ = (g, out, now);
}

/// The window lost its activation: the key-ups and button-ups that follow go elsewhere, so nothing is held down.
pub fn on_deactivate(g: &mut OrderGlue) {
    g.inputs.modifier_down = false;
    g.inputs.button_down = false;
}

/// Whether a menu this app opened is still open.
pub fn menu_open(g: &OrderGlue) -> bool {
    g.inputs.menu_open
}

/// A menu ended (a pick or a close): the hold of an open menu ends.
pub fn on_menu_closed(g: &mut OrderGlue) {
    g.inputs.menu_open = false;
}

/// The hold of the last evaluation.
pub fn is_held(g: &OrderGlue, now: u64) -> bool {
    let _ = now;
    g.hold.held
}
