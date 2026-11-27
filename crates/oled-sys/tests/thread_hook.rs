//! WK1: the host's thread hook runs first thing in every thread `bound` starts, once per capture (the dock's wake-probe
//! build counts its threads through it). Its own test binary: the hook is once per process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use oled_sys::worker::bound::{run_capture, set_on_thread};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn count() {
    CALLS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn thread_hook_runs_once_per_capture() {
    let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
    let echo = ["/c", "echo hook"];
    run_capture(&cmd, &echo, Duration::from_secs(10), 1024).unwrap();
    assert_eq!(CALLS.load(Ordering::SeqCst), 0, "no hook set: nothing is called");
    assert!(set_on_thread(count));
    assert!(!set_on_thread(count), "the first hook stays");
    for n in 1..=3 {
        let (code, out) = run_capture(&cmd, &echo, Duration::from_secs(10), 1024).unwrap();
        assert_eq!((code, out.starts_with(b"hook")), (0, true));
        assert_eq!(CALLS.load(Ordering::SeqCst), n, "one call per capture");
    }
}
