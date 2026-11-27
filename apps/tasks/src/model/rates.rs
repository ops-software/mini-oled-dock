//! Per-process CPU and I/O rates.
use oled_sysmon::{ProcKey, ProcRow, ProcTable};

pub const CPU_TAU_MS: u32 = 2000;
pub const IO_TAU_MS: u32 = 3000;

/// primed false: CPU and I/O draw "—".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    pub cpu: f32,
    pub cpu_raw: f32,
    pub io_bps: f32,
    pub io_raw_bps: f32,
    pub read_bps: f32,
    pub write_bps: f32,
    pub other_bps: f32,
    pub primed: bool,
}

/// A time-constant smoother in 4 bytes: NaN until the first value (the book's budget leaves no room for more).
#[derive(Clone, Copy, Debug)]
struct Smooth(f32);

impl Smooth {
    const UNPRIMED: Smooth = Smooth(f32::NAN);

    /// The first value primes; then alpha = 1 - exp(-dt/tau). Keeps the value between its two inputs.
    fn push(&mut self, x: f32, dt_ms: u32, tau_ms: u32) -> f32 {
        if !x.is_finite() {
            return if self.0.is_nan() { 0.0 } else { self.0 };
        }
        if self.0.is_nan() {
            self.0 = x;
            return x;
        }
        let alpha = if tau_ms == 0 { 1.0 } else { 1.0 - (-(dt_ms as f64) / tau_ms as f64).exp() };
        let next = (self.0 as f64 + alpha * (x as f64 - self.0 as f64)) as f32;
        self.0 = next.clamp(x.min(self.0), x.max(self.0));
        self.0
    }
}

/// Aligned vectors, sorted by key.
#[derive(Debug, Default)]
pub struct RateBook {
    keys: Vec<ProcKey>,
    rates: Vec<Rates>,
    cpu: Vec<Smooth>,
    io: Vec<Smooth>,
}

/// One row's rates from its previous row (None: first sight, unprimed).
fn step(prev: Option<&ProcRow>, now: &ProcRow, dt_ms: u32, cpus: u32, cpu: &mut Smooth, io: &mut Smooth) -> Rates {
    let Some(prev) = prev.filter(|_| dt_ms > 0) else {
        return Rates::default();
    };
    let secs = dt_ms as f64 / 1000.0;
    let wall = dt_ms as f64 * 10_000.0 * cpus.max(1) as f64;
    let cpu_raw = (now.cpu_100ns.saturating_sub(prev.cpu_100ns) as f64 / wall * 100.0) as f32;
    let per_s = |a: u64, b: u64| (a.saturating_sub(b) as f64 / secs) as f32;
    let read_bps = per_s(now.io_read, prev.io_read);
    let write_bps = per_s(now.io_write, prev.io_write);
    let other_bps = per_s(now.io_other, prev.io_other);
    let io_raw_bps = read_bps + write_bps + other_bps;
    Rates {
        cpu: cpu.push(cpu_raw, dt_ms, CPU_TAU_MS),
        cpu_raw,
        io_bps: io.push(io_raw_bps, dt_ms, IO_TAU_MS),
        io_raw_bps,
        read_bps,
        write_bps,
        other_bps,
        primed: true,
    }
}

impl RateBook {
    pub fn new() -> RateBook {
        RateBook::default()
    }

    /// Rates of every row of now over the last dt_ms; both tables are finished. A key that is gone drops out, a key
    /// that is new (or whose pid was reused, so its created differs) starts unprimed.
    pub fn update(&mut self, prev: &ProcTable, now: &ProcTable, dt_ms: u32, logical_cpus: u32) {
        let same = self.keys.len() == now.rows.len() && self.keys.iter().zip(&now.rows).all(|(k, r)| *k == r.key);
        let mut p = 0;
        let mut prev_of = |key: ProcKey| -> Option<&ProcRow> {
            while p < prev.rows.len() && prev.rows[p].key < key {
                p += 1;
            }
            prev.rows.get(p).filter(|r| r.key == key)
        };
        if same {
            for (i, row) in now.rows.iter().enumerate() {
                let before = prev_of(row.key);
                self.rates[i] = step(before, row, dt_ms, logical_cpus, &mut self.cpu[i], &mut self.io[i]);
            }
            return;
        }
        let n = now.rows.len();
        let (mut keys, mut rates) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let (mut cpu, mut io) = (Vec::with_capacity(n), Vec::with_capacity(n));
        let mut j = 0;
        for row in &now.rows {
            while j < self.keys.len() && self.keys[j] < row.key {
                j += 1;
            }
            let known = j < self.keys.len() && self.keys[j] == row.key;
            let (mut c, mut o) = if known { (self.cpu[j], self.io[j]) } else { (Smooth::UNPRIMED, Smooth::UNPRIMED) };
            rates.push(step(prev_of(row.key), row, dt_ms, logical_cpus, &mut c, &mut o));
            keys.push(row.key);
            cpu.push(c);
            io.push(o);
        }
        self.keys = keys;
        self.rates = rates;
        self.cpu = cpu;
        self.io = io;
    }

    pub fn get(&self, key: ProcKey) -> Option<&Rates> {
        let i = self.keys.binary_search(&key).ok()?;
        self.rates.get(i)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(pid: u32, created: u64) -> ProcKey {
        ProcKey { pid, created }
    }

    fn table(rows: &[ProcRow]) -> ProcTable {
        let mut t = ProcTable::new();
        for r in rows {
            t.push(*r);
        }
        t.finish();
        t
    }

    fn row(pid: u32, created: u64, cpu_100ns: u64, io_read: u64) -> ProcRow {
        ProcRow { key: k(pid, created), cpu_100ns, io_read, ..ProcRow::default() }
    }

    #[test]
    fn rates_first_sample_unprimed() {
        let now = table(&[row(7, 1, 100, 100)]);
        let mut book = RateBook::new();
        book.update(&ProcTable::new(), &now, 1000, 4);
        let r = book.get(k(7, 1)).copied().unwrap();
        assert!(!r.primed);
        assert_eq!(r, Rates::default());
        assert_eq!(book.len(), 1);
        assert!(!book.is_empty());
        assert!(book.get(k(8, 1)).is_none());
    }

    #[test]
    fn rates_cpu_and_io() {
        let prev = table(&[row(7, 1, 1_000, 0)]);
        let now = table(&[row(7, 1, 1_000 + 5_000_000, 2 * 1024 * 1024)]);
        let mut book = RateBook::new();
        book.update(&prev, &now, 1000, 4);
        let r = book.get(k(7, 1)).copied().unwrap();
        assert!(r.primed);
        assert!((r.cpu_raw - 12.5).abs() < 1e-4, "{}", r.cpu_raw);
        assert_eq!(r.cpu, r.cpu_raw);
        assert_eq!(r.io_raw_bps, 2_097_152.0);
        assert_eq!(r.io_bps, 2_097_152.0);
        assert_eq!((r.read_bps, r.write_bps, r.other_bps), (2_097_152.0, 0.0, 0.0));
        // A second, idle second pulls both EMAs down but not to zero in one step.
        let later = table(&[row(7, 1, 1_000 + 5_000_000, 2 * 1024 * 1024)]);
        book.update(&now, &later, 1000, 4);
        let r2 = book.get(k(7, 1)).copied().unwrap();
        assert_eq!(r2.cpu_raw, 0.0);
        assert!(r2.cpu > 0.0 && r2.cpu < r.cpu);
        assert!(r2.io_bps > 0.0 && r2.io_bps < r.io_bps);
    }

    #[test]
    fn rates_drop_gone_and_reused_pid() {
        let a = table(&[row(7, 1, 0, 0), row(9, 1, 0, 0)]);
        let b = table(&[row(7, 1, 1_000_000, 0), row(9, 1, 0, 0)]);
        let mut book = RateBook::new();
        book.update(&a, &b, 1000, 1);
        assert_eq!(book.len(), 2);
        // Pid 9 is gone; pid 7 comes back with a new creation time; pid 11 is new.
        let c = table(&[row(7, 50, 5, 0), row(11, 60, 5, 0)]);
        book.update(&b, &c, 1000, 1);
        assert_eq!(book.len(), 2);
        assert!(book.get(k(9, 1)).is_none());
        assert!(book.get(k(7, 1)).is_none());
        assert!(!book.get(k(7, 50)).unwrap().primed);
        assert!(!book.get(k(11, 60)).unwrap().primed);
    }

    #[test]
    fn rates_zero_dt_and_counter_reset_are_safe() {
        let a = table(&[row(7, 1, 500, 500)]);
        let b = table(&[row(7, 1, 100, 100)]);
        let mut book = RateBook::new();
        book.update(&a, &b, 1000, 0);
        let r = book.get(k(7, 1)).copied().unwrap();
        assert!(r.primed && r.cpu_raw == 0.0 && r.io_raw_bps == 0.0);
        book.update(&a, &b, 0, 4);
        assert!(!book.get(k(7, 1)).unwrap().primed);
    }

    #[test]
    fn ratebook_fits() {
        let rows: Vec<ProcRow> = (0..1000).map(|i| row(i + 1, 1, 0, 0)).collect();
        let (a, b) = (table(&rows), table(&rows));
        let mut book = RateBook::new();
        book.update(&a, &b, 1000, 4);
        book.update(&a, &b, 1000, 4);
        let bytes = book.keys.capacity() * std::mem::size_of::<ProcKey>()
            + book.rates.capacity() * std::mem::size_of::<Rates>()
            + (book.cpu.capacity() + book.io.capacity()) * std::mem::size_of::<Smooth>();
        assert!(bytes <= 64 * 1024, "{bytes}");
    }
}
