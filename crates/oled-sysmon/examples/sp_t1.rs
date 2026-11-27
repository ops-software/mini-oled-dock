//! SP-T1: how long does one process snapshot take on this machine? Read-only: it asks the system for the process list
//! and prints one JSON line with the timings. `--mode raw` times the system call alone, `--mode full` times the whole
//! per-tick work of the sampler (call, walk into a table, diff against the previous table).
use oled_sysmon::snapshot::{next_capacity, INITIAL_BUFFER};
use oled_sysmon::{diff, ProcTable, SnapshotReader};
use std::time::{Duration, Instant};
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

const MISMATCH: i32 = 0xC000_0004_u32 as i32;

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Raw,
    Full,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    reads: u32,
    interval_ms: u64,
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Args, &'static str> {
    let mut out = Args { mode: Mode::Raw, reads: 120, interval_ms: 500 };
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or("a flag is missing its value")?;
        match flag.as_str() {
            "--mode" => {
                out.mode = match value.as_str() {
                    "raw" => Mode::Raw,
                    "full" => Mode::Full,
                    _ => return Err("--mode takes raw or full"),
                }
            }
            "--reads" => {
                out.reads = value.parse().ok().filter(|n| (1..=10_000).contains(n)).ok_or("--reads takes 1..=10000")?
            }
            "--interval-ms" => {
                out.interval_ms = value.parse().ok().filter(|n| *n <= 10_000).ok_or("--interval-ms takes 0..=10000")?
            }
            _ => return Err("unknown flag"),
        }
    }
    Ok(out)
}

/// The value below which p percent of the sorted samples lie (nearest rank).
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len() * p).div_ceil(100).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

fn micros(d: Duration) -> u64 {
    d.as_micros().min(u128::from(u64::MAX)) as u64
}

fn build_number() -> u32 {
    let mut v =
        OSVERSIONINFOW { dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32, ..Default::default() };
    // SAFETY: v is a valid local OSVERSIONINFOW with its size set.
    let _ = unsafe { RtlGetVersion(&mut v) };
    v.dwBuildNumber
}

/// Entries of a snapshot buffer, counted by hopping NextEntryOffset inside the filled bytes.
fn count_entries(bytes: &[u8]) -> u32 {
    let (mut off, mut n) = (0usize, 0u32);
    while off + 4 <= bytes.len() {
        n += 1;
        let next =
            bytes.get(off..off + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0) as usize;
        if next == 0 {
            break;
        }
        off += next;
    }
    n
}

fn json_line(
    mode: &str,
    a: &Args,
    extra: &str,
    processes: u32,
    bytes: u32,
    samples: &mut [u64],
    retries: u32,
) -> String {
    samples.sort_unstable();
    format!(
        "{{\"spike\":\"SP-T1\",\"mode\":\"{mode}\",\"build\":{},\"processes\":{processes},{extra}\"bytes\":{bytes},\"reads\":{},\"p50_us\":{},\"p95_us\":{},\"max_us\":{},\"retries\":{retries}}}",
        build_number(),
        a.reads,
        percentile(samples, 50),
        percentile(samples, 95),
        samples.last().copied().unwrap_or(0)
    )
}

fn run_raw(a: &Args) -> Result<String, String> {
    let mut buf = vec![0u64; INITIAL_BUFFER / 8];
    let (mut samples, mut retries, mut ret_len) = (Vec::with_capacity(a.reads as usize), 0u32, 0u32);
    for i in 0..a.reads {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(a.interval_ms));
        }
        let mut attempts = 0;
        loop {
            attempts += 1;
            let mut ret = 0u32;
            let t = Instant::now();
            // SAFETY: the Vec owns buf.len() x 8 initialised, 8-aligned bytes; the call writes at most that many.
            let st = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                    &mut ret,
                )
            };
            let spent = t.elapsed();
            if st.0 >= 0 {
                samples.push(micros(spent));
                ret_len = ret;
                break;
            }
            if st.0 == MISMATCH && attempts < 4 {
                let want = next_capacity(buf.len() * 8, ret).map_err(|e| e.to_string())?;
                buf = vec![0u64; want / 8];
                retries += 1;
                continue;
            }
            return Err(format!("status {:#010x}", st.0 as u32));
        }
    }
    // SAFETY: the Vec owns buf.len() x 8 initialised bytes and ret_len never exceeds them (the call succeeded).
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), (ret_len as usize).min(buf.len() * 8)) };
    Ok(json_line("raw", a, "", count_entries(bytes), ret_len, &mut samples, retries))
}

fn run_full(a: &Args) -> Result<String, String> {
    let mut reader = SnapshotReader::new();
    let (mut prev, mut now) = (ProcTable::new(), ProcTable::new());
    let (mut added, mut gone) = (Vec::new(), Vec::new());
    // The first read runs the self-test and fills the first table; it is not timed.
    reader.read(&mut prev).map_err(|e| e.to_string())?;
    let (mut samples, mut retries) = (Vec::with_capacity(a.reads as usize), 0u32);
    for _ in 0..a.reads {
        std::thread::sleep(Duration::from_millis(a.interval_ms));
        let t = Instant::now();
        reader.read(&mut now).map_err(|e| e.to_string())?;
        diff(&prev, &now, &mut added, &mut gone);
        samples.push(micros(t.elapsed()));
        retries += u32::from(reader.stats().retries);
        std::mem::swap(&mut prev, &mut now);
    }
    let stats = reader.stats();
    let extra = format!("\"threads\":{},", stats.threads);
    Ok(json_line("full", a, &extra, stats.entries, stats.bytes, &mut samples, retries))
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{{\"error\":\"{e}\"}}");
            std::process::exit(2)
        }
    };
    let result = match args.mode {
        Mode::Raw => run_raw(&args),
        Mode::Full => run_full(&args),
    };
    match result {
        Ok(line) => println!("{line}"),
        Err(e) => {
            println!("{{\"error\":\"{e}\"}}");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Result<Args, &'static str> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn args_parse() {
        assert_eq!(a(&[]).unwrap(), Args { mode: Mode::Raw, reads: 120, interval_ms: 500 });
        assert_eq!(
            a(&["--mode", "full", "--reads", "3", "--interval-ms", "0"]).unwrap(),
            Args { mode: Mode::Full, reads: 3, interval_ms: 0 }
        );
        assert!(a(&["--reads", "0"]).is_err());
        assert!(a(&["--reads", "10001"]).is_err());
        assert!(a(&["--reads", "10000"]).is_ok());
        assert!(a(&["--interval-ms", "10001"]).is_err());
        assert!(a(&["--mode", "slow"]).is_err());
        assert!(a(&["--mode"]).is_err());
        assert!(a(&["--nope", "1"]).is_err());
    }

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!((percentile(&v, 50), percentile(&v, 95), percentile(&v, 100)), (50, 95, 100));
        assert_eq!(percentile(&[7], 95), 7);
        assert_eq!(percentile(&[], 50), 0);
    }

    #[test]
    fn counts_entries_by_hopping() {
        let mut b = vec![0u8; 0x40];
        b[0..4].copy_from_slice(&0x20u32.to_le_bytes());
        assert_eq!(count_entries(&b), 2);
        assert_eq!(count_entries(&[]), 0);
    }
}
