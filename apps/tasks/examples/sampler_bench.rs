//! The sampler's cost on this machine: the same host for N seconds visible, then N seconds hidden.
//!
//! sampler_bench --secs N
//!
//! Prints one JSON line:
//! {"bench":"sampler","build":..,"processes":..,"tick_p50_us":..,"tick_p95_us":..,"path_first_us":..,
//!  "path_p95_us":..,"cpu_ms":..,"hidden_iterations":..,"hidden_wakes":..}
//! cpu_ms is this process's kernel plus user time over the visible phase; hidden_* count the sampler thread's loops
//! and wake-ups over the hidden phase (both must be 0).

const MAX_SECS: u64 = 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArgError {
    Missing,
    Bad(String),
}

fn parse_args(args: &[String]) -> Result<u64, ArgError> {
    match args {
        [] => Err(ArgError::Missing),
        [flag, value] if flag == "--secs" => {
            value.parse::<u64>().ok().filter(|s| (1..=MAX_SECS).contains(s)).ok_or_else(|| ArgError::Bad(value.clone()))
        }
        [other, ..] => Err(ArgError::Bad(other.clone())),
    }
}

/// The value at the fraction p (0..=1) of the sorted samples, nearest rank; 0 for no samples.
fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

struct Report {
    build: u32,
    processes: usize,
    tick_p50_us: u32,
    tick_p95_us: u32,
    path_first_us: u32,
    path_p95_us: u32,
    cpu_ms: u64,
    hidden_iterations: u64,
    hidden_wakes: u64,
}

impl Report {
    fn json(&self) -> String {
        format!(
            "{{\"bench\":\"sampler\",\"build\":{},\"processes\":{},\"tick_p50_us\":{},\"tick_p95_us\":{},\"path_first_us\":{},\"path_p95_us\":{},\"cpu_ms\":{},\"hidden_iterations\":{},\"hidden_wakes\":{}}}",
            self.build,
            self.processes,
            self.tick_p50_us,
            self.tick_p95_us,
            self.path_first_us,
            self.path_p95_us,
            self.cpu_ms,
            self.hidden_iterations,
            self.hidden_wakes
        )
    }
}

#[cfg(windows)]
mod live {
    use super::*;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tasks::engine::cadence::{Control, Refresh};
    use tasks::engine::frame::Frame;
    use tasks::engine::host::{default_samplers, FrameSink, SamplerHost};
    use tasks::engine::need::{Need, NeedSet};
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    #[repr(C)]
    struct OsVersion {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(v: *mut OsVersion) -> i32;
    }

    fn os_build() -> u32 {
        let mut v = OsVersion {
            size: std::mem::size_of::<OsVersion>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd: [0; 128],
        };
        // SAFETY: v is a correctly sized, writable OSVERSIONINFOW.
        let status = unsafe { RtlGetVersion(&mut v) };
        if status == 0 {
            v.build
        } else {
            0
        }
    }

    /// Kernel plus user time of this process, 100 ns units.
    fn cpu_100ns() -> u64 {
        let (mut c, mut e, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the pseudo handle needs no closing; the out pointers are locals.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if ok.is_err() {
            return 0;
        }
        let join = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
        join(k) + join(u)
    }

    struct Wake(Mutex<Sender<()>>);

    impl FrameSink for Wake {
        fn ready(&self) {
            if let Ok(tx) = self.0.lock() {
                let _ = tx.send(());
            }
        }
    }

    fn control(visible: bool) -> Control {
        Control {
            visible,
            foreground: true,
            minimised_or_cloaked: false,
            display_off: false,
            refresh: Refresh::One,
            idle: false,
            need: NeedSet::EMPTY.with(Need::Procs).with(Need::Windows).with(Need::Hung).with(Need::PerCore),
        }
    }

    pub fn run(secs: u64) -> Result<Report, String> {
        let (tx, rx) = channel();
        let host = SamplerHost::start(Box::new(Wake(Mutex::new(tx))), default_samplers(), control(true))
            .map_err(|e| format!("sampler: {e:?}"))?;
        let mut frame = Frame::default();
        let (mut ticks, mut paths): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let mut first_path = None;
        let mut processes = 0;
        let cpu_before = cpu_100ns();
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if rx.recv_timeout(Duration::from_millis(250)).is_err() || !host.take(&mut frame) {
                continue;
            }
            ticks.push(frame.tick_us);
            paths.push(frame.paths_us);
            first_path.get_or_insert(frame.paths_us);
            processes = frame.procs.len();
        }
        let cpu_ms = cpu_100ns().saturating_sub(cpu_before) / 10_000;
        host.set_control(control(false));
        // Let the thread see the change and go quiet before the counters are read.
        std::thread::sleep(Duration::from_millis(500));
        let before = host.stats();
        std::thread::sleep(Duration::from_secs(secs));
        let after = host.stats();
        host.stop();
        ticks.sort_unstable();
        paths.sort_unstable();
        Ok(Report {
            build: os_build(),
            processes,
            tick_p50_us: percentile(&ticks, 0.50),
            tick_p95_us: percentile(&ticks, 0.95),
            path_first_us: first_path.unwrap_or(0),
            path_p95_us: percentile(&paths, 0.95),
            cpu_ms,
            hidden_iterations: after.iterations.saturating_sub(before.iterations),
            hidden_wakes: after.wakes.saturating_sub(before.wakes),
        })
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let secs = match parse_args(&argv) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sampler_bench: {e:?}\nusage: sampler_bench --secs N");
            std::process::exit(2)
        }
    };
    #[cfg(windows)]
    match live::run(secs) {
        Ok(r) => println!("{}", r.json()),
        Err(e) => {
            eprintln!("sampler_bench: {e}");
            std::process::exit(1)
        }
    }
    #[cfg(not(windows))]
    let _ = secs;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse_args(&a("--secs 60")), Ok(60));
        assert_eq!(parse_args(&a("")), Err(ArgError::Missing));
        assert_eq!(parse_args(&a("--secs")), Err(ArgError::Bad("--secs".into())));
        assert_eq!(parse_args(&a("--secs 0")), Err(ArgError::Bad("0".into())));
        assert_eq!(parse_args(&a("--secs x")), Err(ArgError::Bad("x".into())));
        assert_eq!(parse_args(&a("--secs 3601")), Err(ArgError::Bad("3601".into())));
        assert_eq!(parse_args(&a("--n 5")), Err(ArgError::Bad("--n".into())));
        assert_eq!(parse_args(&a("--secs 5 6")), Err(ArgError::Bad("--secs".into())));
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        assert_eq!(percentile(&[], 0.95), 0);
        assert_eq!(percentile(&[7], 0.5), 7);
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!((percentile(&v, 0.5), percentile(&v, 0.95), percentile(&v, 1.0)), (50, 95, 100));
        assert_eq!(percentile(&[1, 2, 3, 4], 0.5), 2);
    }

    #[test]
    fn the_line_is_one_json_object() {
        let r = Report {
            build: 26300,
            processes: 300,
            tick_p50_us: 1,
            tick_p95_us: 2,
            path_first_us: 3,
            path_p95_us: 4,
            cpu_ms: 5,
            hidden_iterations: 0,
            hidden_wakes: 0,
        };
        let line = r.json();
        assert!(!line.contains('\n'));
        assert_eq!(
            line,
            "{\"bench\":\"sampler\",\"build\":26300,\"processes\":300,\"tick_p50_us\":1,\"tick_p95_us\":2,\"path_first_us\":3,\"path_p95_us\":4,\"cpu_ms\":5,\"hidden_iterations\":0,\"hidden_wakes\":0}"
        );
    }
}
