//! The order that does not jump (hysteretic insertion, hold, click guard, automatic sort).
use crate::model::view::ColumnId;

pub type RowId = u32;
pub const RATE_MS: u64 = 2000;
pub const HOLD_POINTER_MS: u64 = 3000;
pub const HOLD_KEY_MS: u64 = 3000;
pub const RELEASE_MS: u64 = 600;
pub const CLICK_GUARD_MS: u64 = 250;
pub const MARGIN: f32 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SortSpec {
    pub floor: f32,
    pub descending: bool,
}

impl SortSpec {
    pub fn for_column(c: ColumnId, descending: bool) -> SortSpec {
        let floor = match c {
            ColumnId::Cpu => 0.5,
            ColumnId::Memory | ColumnId::PrivateBytes | ColumnId::WorkingSet | ColumnId::PeakWorkingSet => 1_048_576.0,
            ColumnId::Io | ColumnId::ReadPerSec | ColumnId::WritePerSec | ColumnId::OtherPerSec => 65_536.0,
            ColumnId::Gpu => 1.0,
            _ => 0.0,
        };
        SortSpec { floor, descending }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OrderState {
    pub order: Vec<RowId>,
    prev_keys: Vec<(RowId, f32)>,
    pub last_move_ms: Option<u64>,
}

#[cfg(test)]
thread_local! {
    static COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Does a key beat another by more than the margin? NaN never beats; every number beats NaN.
fn beats(spec: SortSpec, ka: f32, kb: f32) -> bool {
    if ka.is_nan() {
        return false;
    }
    if kb.is_nan() {
        return true;
    }
    let m = spec.floor.max(MARGIN * kb.abs());
    if spec.descending {
        ka > kb + m
    } else {
        ka < kb - m
    }
}

fn lookup(sorted: &[(RowId, f32)], id: RowId) -> Option<f32> {
    sorted.binary_search_by_key(&id, |e| e.0).ok().map(|i| sorted[i].1)
}

/// Stable sort by key (NaN last); ties keep the old position, then the RowId.
pub fn full_sort(rows: &[RowId], keys: &[f32], spec: SortSpec, state: &mut OrderState, now_ms: u64) {
    let n = rows.len().min(keys.len());
    let mut items: Vec<(RowId, f32, usize)> = (0..n)
        .map(|i| {
            let old = state.order.iter().position(|&r| r == rows[i]).unwrap_or(usize::MAX);
            (rows[i], keys[i], old)
        })
        .collect();
    items.sort_by(|a, b| {
        let by_key = match (a.1.is_nan(), b.1.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => {
                let o = a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal);
                if spec.descending {
                    o.reverse()
                } else {
                    o
                }
            }
        };
        by_key.then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0))
    });
    state.order.clear();
    state.order.extend(items.iter().map(|e| e.0));
    state.prev_keys.clear();
    state.prev_keys.extend(items.iter().map(|e| (e.0, e.1)));
    state.prev_keys.sort_unstable_by_key(|e| e.0);
    state.last_move_ms = Some(now_ms);
}

/// true = positions changed.
pub fn reorder(rows: &[RowId], keys: &[f32], spec: SortSpec, held: bool, state: &mut OrderState, now_ms: u64) -> bool {
    // prev_keys is sorted by RowId; this tick's keys are appended behind it (also sorted) and the old part
    // is drained at the end, so the pass allocates nothing once the capacity has grown.
    let old_len = state.prev_keys.len();
    let n = rows.len().min(keys.len());
    state.prev_keys.extend((0..n).map(|i| (rows[i], keys[i])));
    state.prev_keys[old_len..].sort_unstable_by_key(|e| e.0);

    // (1) drop the rows that are gone, append the new ones in rows order (a row without a previous key is new).
    let before = state.order.len();
    {
        let cur = &state.prev_keys[old_len..];
        state.order.retain(|&r| lookup(cur, r).is_some());
    }
    let mut changed = state.order.len() != before;
    for &r in &rows[..n] {
        if lookup(&state.prev_keys[..old_len], r).is_none() {
            state.order.push(r);
            changed = true;
        }
    }

    // (3) one insertion pass, unless held or inside the rate limit.
    let limited = state.last_move_ms.is_some_and(|t| now_ms.saturating_sub(t) < RATE_MS);
    if !held && !limited {
        let (prev, cur) = state.prev_keys.split_at(old_len);
        let mut swapped = false;
        for i in 1..state.order.len() {
            let mut j = i;
            while j > 0 {
                let (a, b) = (state.order[j], state.order[j - 1]);
                #[cfg(test)]
                COMPARISONS.with(|x| x.set(x.get() + 1));
                let passes = match (lookup(cur, a), lookup(cur, b), lookup(prev, a), lookup(prev, b)) {
                    (Some(ka), Some(kb), Some(pa), Some(pb)) => beats(spec, ka, kb) && beats(spec, pa, pb),
                    _ => false,
                };
                if !passes {
                    break;
                }
                state.order.swap(j, j - 1);
                swapped = true;
                j -= 1;
            }
        }
        if swapped {
            state.last_move_ms = Some(now_ms);
            changed = true;
        }
    }

    // (4) this tick's keys are the next tick's previous keys.
    state.prev_keys.drain(..old_len);
    changed
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoldInputs {
    pub pointer_moved_in_list_ms: Option<u64>,
    pub button_down: bool,
    pub modifier_down: bool,
    pub menu_open: bool,
    pub list_key_ms: Option<u64>,
    pub row_busy: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoldState {
    pub held: bool,
    pub released_at: Option<u64>,
    last_instant_ms: u64,
}

fn instant(i: &HoldInputs) -> bool {
    i.button_down || i.modifier_down || i.menu_open || i.row_busy
}

fn timed_until(i: &HoldInputs) -> Option<u64> {
    let p = i.pointer_moved_in_list_ms.map(|t| t.saturating_add(HOLD_POINTER_MS));
    let k = i.list_key_ms.map(|t| t.saturating_add(HOLD_KEY_MS));
    match (p, k) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Returns held.
pub fn hold(i: &HoldInputs, state: &mut HoldState, now_ms: u64) -> bool {
    let timed = timed_until(i);
    if instant(i) {
        state.last_instant_ms = now_ms;
        state.held = true;
        state.released_at = None;
        return true;
    }
    if timed.is_some_and(|t| now_ms < t) {
        state.held = true;
        state.released_at = None;
        return true;
    }
    if state.held {
        let cleared_at = timed.unwrap_or(0).max(state.last_instant_ms);
        if now_ms >= cleared_at.saturating_add(RELEASE_MS) {
            state.held = false;
            state.released_at = Some(now_ms);
        }
    }
    state.held
}

/// When to call hold() again.
pub fn hold_deadline(i: &HoldInputs, state: &HoldState) -> Option<u64> {
    if instant(i) {
        return None;
    }
    let timed = timed_until(i);
    if state.held {
        let cleared_at = timed.unwrap_or(0).max(state.last_instant_ms);
        return Some(cleared_at.saturating_add(RELEASE_MS));
    }
    let t = timed?.saturating_add(RELEASE_MS);
    match state.released_at {
        Some(r) if r >= t => None,
        _ => Some(t),
    }
}

pub fn click_dropped(now_ms: u64, last_move_under_pointer_ms: Option<u64>) -> bool {
    last_move_under_pointer_ms.is_some_and(|t| now_ms.saturating_sub(t) < CLICK_GUARD_MS)
}

/// Memory or Cpu.
pub fn auto_sort(mem_load_pct: u8, memory_full: bool) -> ColumnId {
    if memory_full || mem_load_pct >= 90 {
        ColumnId::Memory
    } else {
        ColumnId::Cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn cpu() -> SortSpec {
        SortSpec::for_column(ColumnId::Cpu, true)
    }

    fn ids(n: u32) -> Vec<RowId> {
        (0..n).collect()
    }

    /// Calls reorder twice with the same keys so the rows have a previous tick, then returns the state.
    fn primed(rows: &[RowId], keys: &[f32]) -> OrderState {
        let mut s = OrderState::default();
        reorder(rows, keys, cpu(), false, &mut s, 0);
        s
    }

    #[test]
    fn floors_per_column() {
        assert_eq!(SortSpec::for_column(ColumnId::Cpu, true).floor, 0.5);
        assert_eq!(SortSpec::for_column(ColumnId::Memory, true).floor, 1_048_576.0);
        assert_eq!(SortSpec::for_column(ColumnId::PeakWorkingSet, true).floor, 1_048_576.0);
        assert_eq!(SortSpec::for_column(ColumnId::ReadPerSec, false).floor, 65_536.0);
        assert_eq!(SortSpec::for_column(ColumnId::Gpu, true).floor, 1.0);
        assert_eq!(SortSpec::for_column(ColumnId::Pid, false).floor, 0.0);
        assert!(!SortSpec::for_column(ColumnId::Pid, false).descending);
    }

    #[test]
    fn no_swap_inside_the_margin() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for case in 0..2000 {
            let spec = SortSpec::for_column(if case % 2 == 0 { ColumnId::Cpu } else { ColumnId::Pid }, case % 3 != 0);
            let n = 2 + rng.below(12) as u32;
            let rows = ids(n);
            let mut s = OrderState::default();
            let mut prev: Vec<f32> = Vec::new();
            let mut now = 0u64;
            for _ in 0..6 {
                let keys: Vec<f32> = rows.iter().map(|_| (rng.below(40) as f32) * 0.5).collect();
                let old = s.order.clone();
                reorder(&rows, &keys, spec, false, &mut s, now);
                if !prev.is_empty() {
                    let pos = |o: &[RowId], r: RowId| o.iter().position(|&x| x == r).unwrap();
                    for &x in &rows {
                        for &y in &rows {
                            if x != y && pos(&old, x) > pos(&old, y) && pos(&s.order, x) < pos(&s.order, y) {
                                // x moved ahead of y: it must pass y on both ticks.
                                assert!(beats(spec, keys[x as usize], keys[y as usize]));
                                assert!(beats(spec, prev[x as usize], prev[y as usize]));
                            }
                        }
                    }
                }
                prev = keys;
                now += RATE_MS;
            }
        }
    }

    #[test]
    fn needs_two_ticks() {
        let rows = ids(2);
        let mut s = primed(&rows, &[10.0, 10.0]);
        // Row 1 jumps for one tick: nothing moves.
        assert!(!reorder(&rows, &[10.0, 50.0], cpu(), false, &mut s, 2000));
        assert_eq!(s.order, vec![0, 1]);
        // The same keys on the next eligible tick move it.
        assert!(reorder(&rows, &[10.0, 50.0], cpu(), false, &mut s, 4000));
        assert_eq!(s.order, vec![1, 0]);
    }

    #[test]
    fn moves_far_in_one_pass() {
        let rows = ids(12);
        let mut keys = vec![10.0f32; 12];
        let mut s = primed(&rows, &keys);
        keys[11] = 100.0;
        reorder(&rows, &keys, cpu(), false, &mut s, 2000);
        assert!(reorder(&rows, &keys, cpu(), false, &mut s, 4000));
        assert_eq!(s.order[0], 11);
        assert_eq!(s.order.len(), 12);
    }

    #[test]
    fn rate_limit_two_seconds() {
        let rows = ids(3);
        let mut keys = vec![10.0f32, 10.0, 10.0];
        let mut s = primed(&rows, &keys);
        keys[1] = 50.0;
        reorder(&rows, &keys, cpu(), false, &mut s, 100);
        assert!(reorder(&rows, &keys, cpu(), false, &mut s, 2100));
        assert_eq!(s.last_move_ms, Some(2100));
        keys[2] = 90.0;
        reorder(&rows, &keys, cpu(), false, &mut s, 3000);
        assert!(!reorder(&rows, &keys, cpu(), false, &mut s, 4099));
        assert!(reorder(&rows, &keys, cpu(), false, &mut s, 4100));
        assert_eq!(s.order, vec![2, 1, 0]);
    }

    #[test]
    fn held_freezes_positions_but_learns_keys() {
        let rows = ids(2);
        let mut s = primed(&rows, &[10.0, 10.0]);
        assert!(!reorder(&rows, &[10.0, 50.0], cpu(), true, &mut s, 2000));
        assert!(!reorder(&rows, &[10.0, 50.0], cpu(), true, &mut s, 4000));
        assert_eq!(s.order, vec![0, 1]);
        // Released: the keys were learned while held, so the move is immediate.
        assert!(reorder(&rows, &[10.0, 50.0], cpu(), false, &mut s, 6000));
        assert_eq!(s.order, vec![1, 0]);
    }

    #[test]
    fn stable_and_linear_on_sorted_input() {
        let n = 1000u32;
        let rows = ids(n);
        let keys: Vec<f32> = (0..n).map(|i| (n - i) as f32 * 10.0).collect();
        let mut s = primed(&rows, &keys);
        COMPARISONS.with(|c| c.set(0));
        assert!(!reorder(&rows, &keys, cpu(), false, &mut s, 2000));
        assert!(COMPARISONS.with(|c| c.get()) <= n as usize);
        assert_eq!(s.order, rows);
    }

    #[test]
    fn new_rows_append() {
        let mut s = primed(&[1, 2, 3], &[30.0, 20.0, 10.0]);
        assert!(reorder(&[3, 5, 1, 4], &[10.0, 1.0, 30.0, 2.0], cpu(), false, &mut s, 2000));
        assert_eq!(s.order, vec![1, 3, 5, 4]);
    }

    #[test]
    fn nan_sorts_last() {
        let rows = ids(3);
        let keys = [f32::NAN, 5.0, 1.0];
        let mut s = primed(&rows, &keys);
        reorder(&rows, &keys, cpu(), false, &mut s, 2000);
        assert_eq!(s.order, vec![1, 2, 0]);
        let mut f = OrderState::default();
        full_sort(&rows, &keys, cpu(), &mut f, 0);
        assert_eq!(f.order, vec![1, 2, 0]);
        let asc = SortSpec::for_column(ColumnId::Pid, false);
        full_sort(&rows, &keys, asc, &mut f, 0);
        assert_eq!(f.order, vec![2, 1, 0]);
    }

    #[test]
    fn full_sort_ties_keep_old_position() {
        let mut s = OrderState { order: vec![3, 1, 2], ..Default::default() };
        full_sort(&[1, 2, 3, 4], &[5.0, 5.0, 5.0, 5.0], cpu(), &mut s, 77);
        assert_eq!(s.order, vec![3, 1, 2, 4]);
        assert_eq!(s.last_move_ms, Some(77));
    }

    fn inp() -> HoldInputs {
        HoldInputs::default()
    }

    #[test]
    fn hold_by_each_condition() {
        for n in 0..6 {
            let mut i = inp();
            match n {
                0 => i.pointer_moved_in_list_ms = Some(1000),
                1 => i.button_down = true,
                2 => i.modifier_down = true,
                3 => i.menu_open = true,
                4 => i.list_key_ms = Some(1000),
                _ => i.row_busy = true,
            }
            let mut st = HoldState::default();
            assert!(hold(&i, &mut st, 1100), "condition {n}");
        }
        let mut st = HoldState::default();
        assert!(!hold(&inp(), &mut st, 1100));
    }

    #[test]
    fn release_600_ms_after_pointer_quiet() {
        let i = HoldInputs { pointer_moved_in_list_ms: Some(0), ..inp() };
        let mut st = HoldState::default();
        assert!(hold(&i, &mut st, 0));
        assert!(hold(&i, &mut st, 2999));
        assert!(hold(&i, &mut st, 3000));
        assert!(hold(&i, &mut st, 3599));
        assert_eq!(st.released_at, None);
        assert!(!hold(&i, &mut st, 3600));
        assert_eq!(st.released_at, Some(3600));
    }

    #[test]
    fn release_after_button_up() {
        let down = HoldInputs { button_down: true, ..inp() };
        let mut st = HoldState::default();
        assert!(hold(&down, &mut st, 1000));
        assert!(hold(&inp(), &mut st, 1000));
        assert!(hold(&inp(), &mut st, 1599));
        assert!(!hold(&inp(), &mut st, 1600));
        assert_eq!(st.released_at, Some(1600));
    }

    #[test]
    fn deadline_matches_release() {
        let i = HoldInputs { pointer_moved_in_list_ms: Some(0), list_key_ms: Some(500), ..inp() };
        let mut st = HoldState::default();
        assert!(hold(&i, &mut st, 10));
        let d = hold_deadline(&i, &st).unwrap();
        assert_eq!(d, 500 + HOLD_KEY_MS + RELEASE_MS);
        assert!(hold(&i, &mut st, d - 1));
        assert!(!hold(&i, &mut st, d));
        assert_eq!(hold_deadline(&i, &st), None);
        // Instant conditions have no deadline; after they clear it is last instant + release.
        let down = HoldInputs { button_down: true, ..inp() };
        let mut st = HoldState::default();
        hold(&down, &mut st, 100);
        assert_eq!(hold_deadline(&down, &st), None);
        assert_eq!(hold_deadline(&inp(), &st), Some(100 + RELEASE_MS));
        assert!(!hold(&inp(), &mut st, 700));
    }

    #[test]
    fn click_guard_250() {
        assert!(!click_dropped(1000, None));
        assert!(click_dropped(1000, Some(1000)));
        assert!(click_dropped(1249, Some(1000)));
        assert!(!click_dropped(1250, Some(1000)));
    }

    #[test]
    fn auto_sort_memory_at_90() {
        assert_eq!(auto_sort(89, false), ColumnId::Cpu);
        assert_eq!(auto_sort(90, false), ColumnId::Memory);
        assert_eq!(auto_sort(10, true), ColumnId::Memory);
    }
}
