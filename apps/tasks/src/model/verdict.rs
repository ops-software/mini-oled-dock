//! The verdict line: eight rules over the system state and the groups.
use crate::model::group::AppKey;
use crate::model::view::Section;
use std::collections::HashMap;

pub const LOOKING_MS: u64 = 1000;
pub const EXIT_MS: u64 = 5000;
pub const OPEN_FAST_MS: u64 = 10_000;
pub const FAST_DWELL_MS: u64 = 1000;
pub const RENAME_LEAD: f32 = 0.20;
pub const RENAME_MS: u64 = 3000;
pub const SNOOZE_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SysState {
    pub cpu_pct: f32,
    pub mem_load_pct: u8,
    pub commit_pct: f32,
    pub dpc_irq_pct: f32,
    pub logical_cpus: u32,
    pub paused: bool,
    pub read_error: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupStat {
    pub key: AppKey,
    pub section: Section,
    pub cpu: f32,
    pub mem: u64,
    pub io_bps: f32,
    pub hung: bool,
    pub has_windows: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Looking,
    Paused,
    ReadError {
        status: i32,
    },
    /// Rule 1.
    NotResponding {
        app: AppKey,
        secs: u32,
    },
    /// Rule 2.
    MemoryFull {
        top: AppKey,
        bytes: u64,
    },
    /// Rule 3.
    CpuHog {
        top: AppKey,
        pct: u8,
    },
    /// Rule 4 (pct = the busiest group's).
    CpuBusy {
        busiest: Option<AppKey>,
        pct: u8,
    },
    /// Rule 5.
    Drivers {
        pct: u8,
    },
    /// Rule 6.
    CoreBound {
        app: AppKey,
    },
    /// Rule 7.
    IoHeavy {
        top: AppKey,
        bps: u64,
    },
    /// Rule 8; busiest None = "Your PC is idle".
    Calm {
        busiest: Option<AppKey>,
        pct: u8,
    },
}

/// Priority order. CpuHog and CpuBusy are one rule family (rules 3 and 4 share "machine CPU >= 85 % for 3 s"): at
/// most one of them is ever in active(), and more() counts the family once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    NotResponding,
    MemoryFull,
    CpuHog,
    CpuBusy,
    Drivers,
    CoreBound,
    IoHeavy,
}

const CPU_ENTER_PCT: f32 = 85.0;
const HOG_PCT: f32 = 25.0;
const CPU_DWELL_MS: u64 = 3000;
const MEM_DWELL_MS: u64 = 3000;
const DRIVERS_DWELL_MS: u64 = 5000;
const CORE_DWELL_MS: u64 = 10_000;
const IO_DWELL_MS: u64 = 5000;
const IO_HEAVY_BPS: f32 = 50.0 * 1_048_576.0;

/// Dwell and exit bookkeeping of one rule.
#[derive(Clone, Copy, Debug, Default)]
struct Track {
    active: bool,
    true_since: Option<u64>,
    false_since: Option<u64>,
}

impl Track {
    fn step(&mut self, cond: bool, now: u64, dwell: u64) {
        if cond {
            self.false_since = None;
            let since = *self.true_since.get_or_insert(now);
            if !self.active && now.saturating_sub(since) >= dwell {
                self.active = true;
            }
        } else if self.active {
            let since = *self.false_since.get_or_insert(now);
            if now.saturating_sub(since) >= EXIT_MS {
                self.reset();
            }
        } else {
            self.true_since = None;
        }
    }
    fn reset(&mut self) {
        *self = Track::default();
    }
}

/// The app a rule names, with its challenger.
#[derive(Clone, Copy, Debug, Default)]
struct Name {
    key: Option<AppKey>,
    val: f64,
    challenger: Option<(AppKey, u64)>,
}

impl Name {
    fn update(
        &mut self,
        cand: Option<(AppKey, f64)>,
        lookup: impl Fn(AppKey) -> Option<f64>,
        now: u64,
        rename_ms: u64,
    ) {
        let Some((ck, cv)) = cand else {
            self.challenger = None;
            return;
        };
        let current = self.key.and_then(|k| lookup(k).map(|v| (k, v)));
        let Some((k, v)) = current else {
            *self = Name { key: Some(ck), val: cv, challenger: None };
            return;
        };
        self.val = v;
        if ck == k || cv <= v * (1.0 + f64::from(RENAME_LEAD)) {
            self.challenger = None;
            return;
        }
        let since = match self.challenger {
            Some((c, t)) if c == ck => t,
            _ => now,
        };
        if now.saturating_sub(since) >= rename_ms {
            *self = Name { key: Some(ck), val: cv, challenger: None };
        } else {
            self.challenger = Some((ck, since));
        }
    }
    fn clear(&mut self) {
        *self = Name::default();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CpuVariant {
    Hog,
    Busy,
}

#[derive(Clone, Debug)]
pub struct VerdictState {
    opened_ms: u64,
    r1: Track,
    hung: HashMap<AppKey, (u64, u32)>,
    r1_last: Option<(AppKey, u64)>,
    snoozed: HashMap<AppKey, u64>,
    mem: Track,
    mem_name: Name,
    cpu: Track,
    cpu_name: Name,
    variant: CpuVariant,
    below_since: Option<u64>,
    drivers: Track,
    core: Track,
    core_name: Name,
    io: Track,
    io_name: Name,
    calm_name: Name,
    dpc_pct: f32,
}

impl VerdictState {
    pub fn new(opened_ms: u64) -> VerdictState {
        VerdictState {
            opened_ms,
            r1: Track::default(),
            hung: HashMap::new(),
            r1_last: None,
            snoozed: HashMap::new(),
            mem: Track::default(),
            mem_name: Name::default(),
            cpu: Track::default(),
            cpu_name: Name::default(),
            variant: CpuVariant::Busy,
            below_since: None,
            drivers: Track::default(),
            core: Track::default(),
            core_name: Name::default(),
            io: Track::default(),
            io_name: Name::default(),
            calm_name: Name::default(),
            dpc_pct: 0.0,
        }
    }
    pub fn shown(&self) -> Option<Rule> {
        self.active().first().copied()
    }
    /// Active rules besides the shown one ("1 more" chip).
    pub fn more(&self) -> u8 {
        self.active().len().saturating_sub(1) as u8
    }
    /// In priority order, for the chip's cycling.
    pub fn active(&self) -> Vec<Rule> {
        let mut v = Vec::with_capacity(6);
        if self.r1.active {
            v.push(Rule::NotResponding);
        }
        if self.mem.active {
            v.push(Rule::MemoryFull);
        }
        if self.cpu.active {
            v.push(if self.variant == CpuVariant::Hog { Rule::CpuHog } else { Rule::CpuBusy });
        }
        if self.drivers.active {
            v.push(Rule::Drivers);
        }
        if self.core.active {
            v.push(Rule::CoreBound);
        }
        if self.io.active {
            v.push(Rule::IoHeavy);
        }
        v
    }
    /// [Wait].
    pub fn snooze(&mut self, app: AppKey, now_ms: u64) {
        self.snoozed.insert(app, now_ms + SNOOZE_MS);
        self.hung.remove(&app);
        if self.r1_last.is_some_and(|(k, _)| k == app) {
            self.r1.reset();
            self.r1_last = None;
        }
    }
    fn fast(&self, now: u64) -> bool {
        now.saturating_sub(self.opened_ms) < OPEN_FAST_MS
    }
    fn dwell(&self, normal: u64, now: u64) -> u64 {
        if self.fast(now) {
            FAST_DWELL_MS
        } else {
            normal
        }
    }
    fn rename_ms(&self, now: u64) -> u64 {
        self.dwell(RENAME_MS, now)
    }
}

/// The best group by a metric; ties go to the smaller AppKey.
fn best(groups: &[GroupStat], metric: impl Fn(&GroupStat) -> f64) -> Option<(AppKey, f64)> {
    let mut out: Option<(AppKey, f64)> = None;
    for g in groups {
        let m = metric(g);
        out = match out {
            Some((k, v)) if v > m || (v == m && k <= g.key) => Some((k, v)),
            _ => Some((g.key, m)),
        };
    }
    out
}

fn pct8(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn find<'a>(groups: &'a [GroupStat], metric: impl Fn(&GroupStat) -> f64 + 'a) -> impl Fn(AppKey) -> Option<f64> + 'a {
    move |k| groups.iter().find(|g| g.key == k).map(&metric)
}

fn update_hung(state: &mut VerdictState, groups: &[GroupStat], now: u64) {
    state.snoozed.retain(|_, until| *until > now);
    let mut next = HashMap::with_capacity(state.hung.len());
    for g in groups {
        if g.has_windows && g.hung && !state.snoozed.contains_key(&g.key) {
            let (first, n) = state.hung.get(&g.key).copied().unwrap_or((now, 0));
            next.insert(g.key, (first, n + 1));
        }
    }
    state.hung = next;
}

pub fn judge(sys: &SysState, groups: &[GroupStat], state: &mut VerdictState, now_ms: u64) -> Verdict {
    let now = now_ms;
    let rename = state.rename_ms(now);

    // Rule 1: two consecutive hung calls; the longest hung app is named.
    update_hung(state, groups, now);
    let longest = state.hung.iter().filter(|(_, (_, n))| *n >= 2).min_by_key(|(k, (first, _))| (*first, **k));
    if let Some((k, (first, _))) = longest {
        state.r1_last = Some((*k, *first));
    }
    let r1_cond = longest.is_some();
    let r1_dwell = 0;
    state.r1.step(r1_cond, now, r1_dwell);
    if !state.r1.active && !r1_cond {
        state.r1_last = None;
    }

    // Rule 2.
    state.mem_name.update(best(groups, |g| g.mem as f64), find(groups, |g| g.mem as f64), now, rename);
    let mem_cond = !groups.is_empty() && (sys.mem_load_pct >= 90 || sys.commit_pct >= 90.0);
    let d = state.dwell(MEM_DWELL_MS, now);
    state.mem.step(mem_cond, now, d);

    // Rules 3 and 4: one family.
    let top_cpu = best(groups, |g| f64::from(g.cpu));
    state.cpu_name.update(top_cpu, find(groups, |g| f64::from(g.cpu)), now, rename);
    let top_is_hog = top_cpu.is_some_and(|(_, v)| v >= f64::from(HOG_PCT));
    let cpu_cond = sys.cpu_pct >= CPU_ENTER_PCT;
    if !state.cpu.active {
        state.variant = if top_is_hog { CpuVariant::Hog } else { CpuVariant::Busy };
        state.below_since = None;
    } else if state.variant == CpuVariant::Busy {
        if top_is_hog {
            state.variant = CpuVariant::Hog;
        }
    } else if top_is_hog {
        state.below_since = None;
    } else {
        let since = *state.below_since.get_or_insert(now);
        if now.saturating_sub(since) >= EXIT_MS {
            state.variant = CpuVariant::Busy;
            state.below_since = None;
        }
    }
    let d = state.dwell(CPU_DWELL_MS, now);
    state.cpu.step(cpu_cond, now, d);

    // Rule 5.
    if sys.dpc_irq_pct >= 10.0 {
        state.dpc_pct = sys.dpc_irq_pct;
    }
    let d = state.dwell(DRIVERS_DWELL_MS, now);
    state.drivers.step(sys.dpc_irq_pct >= 10.0, now, d);

    // Rule 6.
    let cores = f64::from(sys.logical_cpus);
    let core_metric = move |g: &GroupStat| f64::from(g.cpu) * cores;
    let top_core = best(groups, core_metric);
    state.core_name.update(top_core, find(groups, core_metric), now, rename);
    let core_cond = top_core.is_some_and(|(_, v)| v >= 95.0);
    let d = state.dwell(CORE_DWELL_MS, now);
    state.core.step(core_cond, now, d);

    // Rule 7.
    let top_io = best(groups, |g| f64::from(g.io_bps));
    state.io_name.update(top_io, find(groups, |g| f64::from(g.io_bps)), now, rename);
    let io_cond = top_io.is_some_and(|(_, v)| v >= f64::from(IO_HEAVY_BPS));
    let d = state.dwell(IO_DWELL_MS, now);
    state.io.step(io_cond, now, d);

    // Rule 8's name.
    match top_cpu {
        Some((_, v)) if v.round() >= 1.0 => {
            state.calm_name.update(top_cpu, find(groups, |g| f64::from(g.cpu)), now, rename);
        }
        _ => state.calm_name.clear(),
    }

    if let Some(status) = sys.read_error {
        return Verdict::ReadError { status };
    }
    if sys.paused {
        return Verdict::Paused;
    }
    if now.saturating_sub(state.opened_ms) < LOOKING_MS {
        return Verdict::Looking;
    }
    for rule in state.active() {
        if let Some(v) = verdict_of(rule, state, now) {
            return v;
        }
    }
    let busiest = state.calm_name.key;
    Verdict::Calm { busiest, pct: busiest.map_or(0, |_| pct8(state.calm_name.val)) }
}

/// The sentence of an active rule; None only when it has no app to name (the next rule is then tried).
fn verdict_of(rule: Rule, st: &VerdictState, now: u64) -> Option<Verdict> {
    Some(match rule {
        Rule::NotResponding => {
            let (app, first) = st.r1_last?;
            Verdict::NotResponding { app, secs: (now.saturating_sub(first) / 1000) as u32 }
        }
        Rule::MemoryFull => Verdict::MemoryFull { top: st.mem_name.key?, bytes: st.mem_name.val as u64 },
        Rule::CpuHog => Verdict::CpuHog { top: st.cpu_name.key?, pct: pct8(st.cpu_name.val) },
        Rule::CpuBusy => Verdict::CpuBusy { busiest: st.cpu_name.key, pct: pct8(st.cpu_name.val) },
        Rule::Drivers => Verdict::Drivers { pct: pct8(f64::from(st.dpc_pct)) },
        Rule::CoreBound => Verdict::CoreBound { app: st.core_name.key? },
        Rule::IoHeavy => Verdict::IoHeavy { top: st.io_name.key?, bps: st.io_name.val as u64 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::group::ImageId;

    const T0: u64 = 20_000; // well past the opening seconds
    const MIB: f32 = 1_048_576.0;

    fn k(n: u32) -> AppKey {
        AppKey::Image(ImageId(n))
    }
    fn g(n: u32, cpu: f32) -> GroupStat {
        GroupStat { key: k(n), section: Section::Apps, cpu, mem: 1 << 20, io_bps: 0.0, hung: false, has_windows: true }
    }
    fn sys(cpu: f32) -> SysState {
        SysState { cpu_pct: cpu, logical_cpus: 2, ..SysState::default() }
    }
    fn same_kind(a: &Verdict, b: &Verdict) -> bool {
        std::mem::discriminant(a) == std::mem::discriminant(b)
    }

    /// Feeds a constant input at 1 s steps from t0 for n ticks; returns the verdict of each tick.
    fn feed(st: &mut VerdictState, s: &SysState, gs: &[GroupStat], t0: u64, n: u64) -> Vec<Verdict> {
        (0..n).map(|i| judge(s, gs, st, t0 + i * 1000)).collect()
    }

    /// The rule must show exactly at its dwell, and leave 5 s after the condition clears.
    fn table(on_sys: SysState, on: Vec<GroupStat>, dwell_s: u64, expect: Verdict) {
        let shown = |v: &Verdict| same_kind(v, &expect);
        let mut st = VerdictState::new(0);
        let off_sys = sys(10.0);
        let off = vec![g(1, 5.0)];
        let v = feed(&mut st, &on_sys, &on, T0, dwell_s + 1);
        for (i, x) in v.iter().enumerate() {
            assert_eq!(shown(x), i as u64 >= dwell_s, "enter, tick {i}: {x:?}");
        }
        assert_eq!(v[dwell_s as usize], expect);
        let t1 = T0 + (dwell_s + 1) * 1000;
        let v = feed(&mut st, &off_sys, &off, t1, 6);
        for (i, x) in v.iter().enumerate() {
            assert_eq!(shown(x), i < 5, "exit, tick {i}: {x:?}");
        }
    }

    #[test]
    fn r1_not_responding() {
        let mut st = VerdictState::new(0);
        let mut h = g(1, 1.0);
        h.hung = true;
        let v0 = judge(&sys(10.0), &[h], &mut st, T0);
        assert!(matches!(v0, Verdict::Calm { .. }), "{v0:?}");
        let v1 = judge(&sys(10.0), &[h], &mut st, T0 + 1000);
        assert_eq!(v1, Verdict::NotResponding { app: k(1), secs: 1 });
        let v = judge(&sys(10.0), &[h], &mut st, T0 + 4000);
        assert_eq!(v, Verdict::NotResponding { app: k(1), secs: 4 });
        let ok = [g(1, 1.0)];
        for i in 0..5 {
            let v = judge(&sys(10.0), &ok, &mut st, T0 + 5000 + i * 1000);
            assert!(matches!(v, Verdict::NotResponding { .. }), "tick {i}");
        }
        assert!(matches!(judge(&sys(10.0), &ok, &mut st, T0 + 10_000), Verdict::Calm { .. }));
    }

    #[test]
    fn r1_needs_windows_and_consecutive_calls() {
        let mut st = VerdictState::new(0);
        let mut h = g(1, 1.0);
        h.hung = true;
        let mut nowin = h;
        nowin.has_windows = false;
        for i in 0..4 {
            assert!(matches!(judge(&sys(10.0), &[nowin], &mut st, T0 + i * 1000), Verdict::Calm { .. }));
        }
        // hung, healthy, hung: never two in a row
        for (i, hung) in [true, false, true, false].into_iter().enumerate() {
            let mut x = h;
            x.hung = hung;
            let v = judge(&sys(10.0), &[x], &mut st, T0 + 10_000 + i as u64 * 1000);
            assert!(matches!(v, Verdict::Calm { .. }), "tick {i}");
        }
    }

    #[test]
    fn r1_names_the_longest_hung() {
        let mut st = VerdictState::new(0);
        let (mut a, mut b) = (g(1, 1.0), g(2, 1.0));
        b.hung = true;
        judge(&sys(10.0), &[a, b], &mut st, T0);
        a.hung = true;
        judge(&sys(10.0), &[a, b], &mut st, T0 + 1000);
        let v = judge(&sys(10.0), &[a, b], &mut st, T0 + 2000);
        assert_eq!(v, Verdict::NotResponding { app: k(2), secs: 2 });
    }

    #[test]
    fn r2_memory_full() {
        let on = SysState { mem_load_pct: 92, ..sys(10.0) };
        let mut big = g(2, 1.0);
        big.mem = 5 << 30;
        table(on, vec![g(1, 1.0), big], 3, Verdict::MemoryFull { top: k(2), bytes: 5 << 30 });
        let on = SysState { commit_pct: 91.0, ..sys(10.0) };
        table(on, vec![big], 3, Verdict::MemoryFull { top: k(2), bytes: 5 << 30 });
    }

    #[test]
    fn r3_cpu_hog() {
        table(sys(90.0), vec![g(1, 30.0), g(2, 10.0)], 3, Verdict::CpuHog { top: k(1), pct: 30 });
    }

    #[test]
    fn r4_cpu_busy() {
        table(sys(90.0), vec![g(1, 20.0), g(2, 10.0)], 3, Verdict::CpuBusy { busiest: Some(k(1)), pct: 20 });
        table(sys(90.0), vec![], 3, Verdict::CpuBusy { busiest: None, pct: 0 });
    }

    #[test]
    fn r5_drivers() {
        let on = SysState { dpc_irq_pct: 12.4, ..sys(10.0) };
        table(on, vec![g(1, 1.0)], 5, Verdict::Drivers { pct: 12 });
    }

    #[test]
    fn r6_core_bound() {
        // 8 logical cpus: 12 % of the machine is 96 % of one core
        table(
            SysState { logical_cpus: 8, ..sys(20.0) },
            vec![g(1, 12.0), g(2, 3.0)],
            10,
            Verdict::CoreBound { app: k(1) },
        );
    }

    #[test]
    fn r7_io_heavy() {
        let mut io = g(1, 1.0);
        io.io_bps = 60.0 * MIB;
        table(sys(10.0), vec![io, g(2, 1.0)], 5, Verdict::IoHeavy { top: k(1), bps: (60.0 * MIB) as u64 });
    }

    #[test]
    fn r8_calm() {
        let mut st = VerdictState::new(0);
        let v = judge(&sys(10.0), &[g(1, 0.4), g(2, 3.4)], &mut st, T0);
        assert_eq!(v, Verdict::Calm { busiest: Some(k(2)), pct: 3 });
        let v = judge(&sys(1.0), &[g(1, 0.4)], &mut st, T0 + 1000);
        assert_eq!(v, Verdict::Calm { busiest: None, pct: 0 });
    }

    #[test]
    fn priority_order() {
        let mut st = VerdictState::new(0);
        let mut h = g(1, 30.0);
        h.hung = true;
        let v = feed(&mut st, &sys(90.0), &[h], T0, 5);
        assert_eq!(v[4], Verdict::NotResponding { app: k(1), secs: 4 });
        assert_eq!(st.shown(), Some(Rule::NotResponding));
        assert_eq!(st.more(), 1);
        assert_eq!(st.active(), vec![Rule::NotResponding, Rule::CpuHog]);
    }

    #[test]
    fn looking_first_second() {
        let mut st = VerdictState::new(5000);
        let mut h = g(1, 1.0);
        h.hung = true;
        assert_eq!(judge(&sys(10.0), &[h], &mut st, 5000), Verdict::Looking);
        assert_eq!(judge(&sys(10.0), &[h], &mut st, 5999), Verdict::Looking);
        assert!(matches!(judge(&sys(10.0), &[h], &mut st, 6000), Verdict::NotResponding { .. }));
    }

    #[test]
    fn paused_and_read_error_win() {
        let mut st = VerdictState::new(0);
        let mut h = g(1, 1.0);
        h.hung = true;
        feed(&mut st, &sys(10.0), &[h], T0, 3);
        let p = SysState { paused: true, ..sys(10.0) };
        assert_eq!(judge(&p, &[h], &mut st, T0 + 3000), Verdict::Paused);
        let e = SysState { paused: true, read_error: Some(-5), ..sys(10.0) };
        assert_eq!(judge(&e, &[h], &mut st, T0 + 4000), Verdict::ReadError { status: -5 });
        assert!(matches!(judge(&sys(10.0), &[h], &mut st, T0 + 5000), Verdict::NotResponding { .. }));
    }

    #[test]
    fn snooze_hides_one_app_30_s() {
        let mut st = VerdictState::new(0);
        let (mut a, mut b) = (g(1, 1.0), g(2, 1.0));
        a.hung = true;
        feed(&mut st, &sys(10.0), &[a, b], T0, 3);
        assert!(matches!(st.shown(), Some(Rule::NotResponding)));
        st.snooze(k(1), T0 + 2000);
        assert!(matches!(judge(&sys(10.0), &[a, b], &mut st, T0 + 3000), Verdict::Calm { .. }));
        // a second hung app is not hidden
        b.hung = true;
        judge(&sys(10.0), &[a, b], &mut st, T0 + 4000);
        let v = judge(&sys(10.0), &[a, b], &mut st, T0 + 5000);
        assert!(matches!(v, Verdict::NotResponding { app, .. } if app == k(2)), "{v:?}");
        b.hung = false;
        for i in 0..5 {
            judge(&sys(10.0), &[a, b], &mut st, T0 + 6000 + i * 1000);
        }
        assert!(matches!(judge(&sys(10.0), &[a, b], &mut st, T0 + 31_000), Verdict::Calm { .. }));
        // 30 s after the snooze it counts again
        judge(&sys(10.0), &[a, b], &mut st, T0 + 32_000);
        let v = judge(&sys(10.0), &[a, b], &mut st, T0 + 33_000);
        assert!(matches!(v, Verdict::NotResponding { app, .. } if app == k(1)), "{v:?}");
    }

    #[test]
    fn no_group_no_named_rule() {
        let mut st = VerdictState::new(0);
        let on = SysState { mem_load_pct: 95, ..sys(10.0) };
        for v in feed(&mut st, &on, &[], T0, 6) {
            assert_eq!(v, Verdict::Calm { busiest: None, pct: 0 });
        }
    }

    #[test]
    fn cpu_family_is_one_rule() {
        let mut st = VerdictState::new(0);
        let mut shown = Vec::new();
        for t in 0..20u64 {
            let top = if t >= 10 { 30.0 } else { 20.0 };
            let v = judge(&sys(90.0), &[g(1, top), g(2, 5.0)], &mut st, T0 + t * 1000);
            assert_eq!(st.more(), 0, "tick {t}");
            shown.push(v);
        }
        assert!(matches!(shown[2], Verdict::Calm { .. }));
        assert_eq!(shown[3], Verdict::CpuBusy { busiest: Some(k(1)), pct: 20 });
        assert_eq!(shown[9], Verdict::CpuBusy { busiest: Some(k(1)), pct: 20 });
        assert_eq!(shown[10], Verdict::CpuHog { top: k(1), pct: 30 });
        let changes = shown[3..].windows(2).filter(|w| !same_kind(&w[0], &w[1])).count();
        assert_eq!(changes, 1);
    }

    #[test]
    fn hog_holds_through_a_short_dip() {
        let run = |dip_s: u64| {
            let mut st = VerdictState::new(0);
            let mut out = Vec::new();
            for t in 0..30u64 {
                let top = if (10..10 + dip_s).contains(&t) { 24.0 } else { 30.0 };
                out.push(judge(&sys(90.0), &[g(1, top)], &mut st, T0 + t * 1000));
            }
            out
        };
        for v in &run(3)[3..] {
            assert!(matches!(v, Verdict::CpuHog { .. }), "{v:?}");
        }
        let v = run(6);
        assert!(matches!(v[14], Verdict::CpuHog { .. }));
        assert!(matches!(v[15], Verdict::CpuBusy { .. }), "{:?}", v[15]);
        assert!(matches!(v[16], Verdict::CpuHog { .. }));
    }

    #[test]
    fn family_exit_is_one_exit() {
        for top in [20.0, 30.0] {
            let mut st = VerdictState::new(0);
            feed(&mut st, &sys(90.0), &[g(1, top)], T0, 6);
            assert!(st.shown().is_some());
            let v = feed(&mut st, &sys(50.0), &[g(1, top)], T0 + 6000, 6);
            for (i, x) in v.iter().enumerate() {
                assert_eq!(matches!(x, Verdict::Calm { .. }), i >= 5, "top {top} tick {i}: {x:?}");
            }
        }
    }

    #[test]
    fn name_holds_inside_20_percent() {
        let mut st = VerdictState::new(0);
        judge(&sys(10.0), &[g(1, 12.0), g(2, 10.0)], &mut st, T0);
        for i in 1..20 {
            let v = judge(&sys(10.0), &[g(1, 12.0), g(2, 14.3)], &mut st, T0 + i * 1000);
            assert_eq!(v, Verdict::Calm { busiest: Some(k(1)), pct: 12 }, "tick {i}");
        }
    }

    #[test]
    fn name_switches_after_3_s_lead() {
        let mut st = VerdictState::new(0);
        judge(&sys(10.0), &[g(1, 12.0), g(2, 10.0)], &mut st, T0);
        let lead = [g(1, 12.0), g(2, 15.0)];
        for i in 1..=3 {
            let v = judge(&sys(10.0), &lead, &mut st, T0 + i * 1000);
            assert_eq!(v, Verdict::Calm { busiest: Some(k(1)), pct: 12 }, "tick {i}");
        }
        // the lead began at T0 + 1 s
        let v = judge(&sys(10.0), &lead, &mut st, T0 + 4000);
        assert_eq!(v, Verdict::Calm { busiest: Some(k(2)), pct: 15 });
        // a lead that lapses starts over
        let mut st = VerdictState::new(0);
        judge(&sys(10.0), &[g(1, 12.0), g(2, 10.0)], &mut st, T0);
        judge(&sys(10.0), &lead, &mut st, T0 + 1000);
        judge(&sys(10.0), &lead, &mut st, T0 + 2000);
        judge(&sys(10.0), &[g(1, 12.0), g(2, 12.0)], &mut st, T0 + 3000);
        judge(&sys(10.0), &lead, &mut st, T0 + 4000);
        let v = judge(&sys(10.0), &lead, &mut st, T0 + 6000);
        assert_eq!(v, Verdict::Calm { busiest: Some(k(1)), pct: 12 });
    }

    #[test]
    fn named_app_that_left_is_replaced_at_once() {
        let mut st = VerdictState::new(0);
        judge(&sys(10.0), &[g(1, 12.0), g(2, 10.0)], &mut st, T0);
        let v = judge(&sys(10.0), &[g(2, 10.0)], &mut st, T0 + 1000);
        assert_eq!(v, Verdict::Calm { busiest: Some(k(2)), pct: 10 });
    }

    #[test]
    fn open_answers_in_one_second() {
        let mut st = VerdictState::new(0);
        let gs = [g(1, 30.0)];
        assert_eq!(judge(&sys(90.0), &gs, &mut st, 0), Verdict::Looking);
        assert_eq!(judge(&sys(90.0), &gs, &mut st, 1000), Verdict::CpuHog { top: k(1), pct: 30 });
        assert_eq!(judge(&sys(90.0), &gs, &mut st, 2000), Verdict::CpuHog { top: k(1), pct: 30 });
        // after the opening seconds the full dwell applies again
        let mut st = VerdictState::new(0);
        judge(&sys(10.0), &gs, &mut st, 10_000);
        assert!(matches!(judge(&sys(90.0), &gs, &mut st, 11_000), Verdict::Calm { .. }));
        assert!(matches!(judge(&sys(90.0), &gs, &mut st, 13_000), Verdict::Calm { .. }));
        assert!(matches!(judge(&sys(90.0), &gs, &mut st, 14_000), Verdict::CpuHog { .. }));
    }

    #[test]
    fn open_keeps_two_calls_for_r1() {
        let mut st = VerdictState::new(0);
        let mut h = g(1, 1.0);
        h.hung = true;
        judge(&sys(10.0), &[h], &mut st, 1000);
        assert!(matches!(judge(&sys(10.0), &[h], &mut st, 1500), Verdict::NotResponding { .. }));
        let mut st = VerdictState::new(0);
        assert!(matches!(judge(&sys(10.0), &[h], &mut st, 1000), Verdict::Calm { .. }));
    }

    #[test]
    fn flap_free_on_noise() {
        for seed in 1..=20u64 {
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = move || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 11) as f32 / (1u64 << 53) as f32
            };
            let mut st = VerdictState::new(0);
            let mut last_name: Option<AppKey> = None;
            let mut last_change: Option<u64> = None;
            for i in 0..600u64 {
                let t = i * 1000;
                let machine = 2.0 + rnd() * 7.0;
                let a = 2.0 + rnd() * 7.0;
                let b = a * (0.85 + rnd() * 0.30);
                let v = judge(&sys(machine), &[g(1, a), g(2, b)], &mut st, t);
                match v {
                    Verdict::Looking => {}
                    Verdict::Calm { busiest, .. } => {
                        if last_name.is_some() && busiest != last_name {
                            if let Some(c) = last_change {
                                assert!(t - c >= RENAME_MS, "seed {seed} tick {i}: renamed twice within 3 s");
                            }
                            last_change = Some(t);
                        }
                        last_name = busiest;
                    }
                    other => panic!("seed {seed} tick {i}: shown {other:?}"),
                }
            }
        }
    }
}
