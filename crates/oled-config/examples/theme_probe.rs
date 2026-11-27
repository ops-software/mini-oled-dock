//! Wake-up probe for the theme file: opens a ThemeWatch, waits on its change handle, and on each signal re-arms it,
//! sleeps 60 ms, reloads and prints "<QPC ms> generation <n>". Exits after 60 s. Reads only; never creates anything.

use std::time::{Duration, Instant};

use oled_config::theme::ThemeWatch;
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::WaitForSingleObject;

fn qpc_ms() -> f64 {
    let (mut freq, mut now) = (0i64, 0i64);
    // SAFETY: both pointers are valid for the call; the calls only write the two integers.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
        let _ = QueryPerformanceCounter(&mut now);
    }
    if freq <= 0 {
        return 0.0;
    }
    now as f64 * 1000.0 / freq as f64
}

fn main() {
    let mut watch = ThemeWatch::open();
    let Some(handle) = watch.change_handle() else {
        eprintln!("no theme folder to watch");
        return;
    };
    println!("{:.3} generation {}", qpc_ms(), watch.current().generation);
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let ms = left.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: the handle is owned by `watch`, which outlives this call.
        if unsafe { WaitForSingleObject(handle, ms) } != WAIT_OBJECT_0 {
            break;
        }
        watch.on_signaled();
        std::thread::sleep(Duration::from_millis(60));
        if let Some(t) = watch.reload() {
            println!("{:.3} generation {}", qpc_ms(), t.generation);
        }
    }
}
