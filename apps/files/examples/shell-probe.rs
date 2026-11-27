//! shell-probe: the shell host as the live tester measures it (P1-files-engine LIVE-F2 steps 3 and 4). A release-built
//! tool, never shipped. Every mode works inside --root, an existing %TEMP%\mod-files-live-* folder (LiveRoot::at), and
//! prints one "probe: ..." line per fact (the host's pid first):
//!   shell-probe icons --root <dir> [--n 200] [--types 40] [--px 32]   n files over that many types, their icons
//!   shell-probe thumbs --root <dir> [--n 20] [--px 96]                 n small bitmaps, their thumbnails
//!   shell-probe menu-build --root <dir> --file <path>                   the file's menu built, never shown
//!   shell-probe menu-show --root <dir> --file <path> [--dismiss-ms 1500]
//!       the only mode that shows UI: the menu at the cursor, which the host dismisses itself after that many ms
//!       (no input is ever sent); "no menu" when it never opened
//!   shell-probe idle --root <dir> [--secs 130]                          one request, then waits for the host's idle exit
//!   shell-probe step3 --root <dir> [--file <path>] [--secs 130]
//!       LIVE-F2 step 3 on one host: icons of 200 files over 40 types, thumbnails of 20 bitmaps and a menu build (never
//!       shown), "probe: idle from now" at the last reply, then the wait for the idle exit; the probe never ends the
//!       host itself (each other mode starts a host of its own)
//! files.exe is the one next to the examples folder, or --exe <path>.

#[path = "common/live.rs"]
mod live;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use files_engine::shell_client::{HostState, ShellClient, ShellConfig, ShellReply};
use oled_trust::frame::files::{Gen, ItemRef, ReqId, ShellEvt, ShellReq, WidePath};

/// Forty common file types, for icons of real handlers.
const TYPES: [&str; 40] = [
    "txt", "pdf", "docx", "xlsx", "pptx", "jpg", "png", "gif", "bmp", "svg", "mp3", "wav", "mp4", "mkv", "avi", "zip",
    "7z", "rar", "iso", "exe", "dll", "msi", "bat", "cmd", "ps1", "py", "rs", "js", "html", "css", "json", "xml",
    "csv", "md", "log", "ini", "ttf", "lnk", "url", "rtf",
];

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn num(args: &[String], name: &str, default: u64) -> u64 {
    arg(args, name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn item(p: &Path) -> Option<ItemRef> {
    WidePath::parse(p.to_str()?).ok().map(ItemRef::Path)
}

/// A 64x64 24-bit bitmap file of one colour.
fn bmp(seed: u8) -> Vec<u8> {
    let (w, h) = (64u32, 64u32);
    let size = 54 + w * h * 3;
    let mut b = Vec::with_capacity(size as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&w.to_le_bytes());
    b.extend_from_slice(&h.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    for _ in 0..w * h {
        b.extend_from_slice(&[seed, 255 - seed, 128]);
    }
    b
}

/// Drains until done accepts a reply or the time runs out; every reply seen.
fn until(c: &ShellClient, within: Duration, done: impl Fn(&ShellReply) -> bool) -> Vec<ShellReply> {
    let end = Instant::now() + within;
    let mut got = Vec::new();
    while Instant::now() < end {
        for r in c.drain() {
            let last = done(&r);
            got.push(r);
            if last {
                return got;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    got
}

fn host_pid(c: &ShellClient) -> u32 {
    c.prespawn();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let HostState::Ready { pid } = c.state() {
            println!("probe: host {pid}");
            return pid;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("probe: no host");
    std::process::exit(1);
}

fn ends(req: ReqId) -> impl Fn(&ShellReply) -> bool {
    move |r| match r {
        ShellReply::Fallback { req: q, .. } => *q == req,
        ShellReply::Evt(
            ShellEvt::ReqDone { req: q } | ShellEvt::MenuDone { req: q, .. } | ShellEvt::Failed { req: q, .. },
        ) => *q == req,
        _ => false,
    }
}

/// Icons or thumbnails of paths, 50 per request; prints the count and the time.
fn images(c: &ShellClient, paths: &[PathBuf], px: u16, thumbs: bool) {
    let t = Instant::now();
    let mut count = 0;
    for chunk in paths.chunks(50) {
        let items: Vec<ItemRef> = chunk.iter().filter_map(|p| item(p)).collect();
        let req = c.next_req();
        c.request(if thumbs {
            ShellReq::Thumbs { req, gen: Gen(1), items, px, cache_only: false }
        } else {
            ShellReq::Icons { req, gen: Gen(1), items, px }
        });
        let got = until(c, Duration::from_secs(60), ends(req));
        count +=
            got.iter().filter(|r| matches!(r, ShellReply::Evt(ShellEvt::Icon { .. } | ShellEvt::Thumb { .. }))).count();
        if let Some(ShellReply::Fallback { why, .. }) = got.last() {
            println!("probe: fallback {why:?}");
        }
    }
    let what = if thumbs { "thumbs" } else { "icons" };
    println!("probe: {what} {count} of {} in {} ms", paths.len(), t.elapsed().as_millis());
}

/// n files over that many of TYPES (at most 40), for icons.
fn icon_files(live: &live::LiveRoot, n: usize, types: usize) -> Vec<PathBuf> {
    let types = types.clamp(1, TYPES.len());
    (0..n).map(|i| live.file(&format!("icons\\i{i:05}.{}", TYPES[i % types]), b"x")).collect()
}

/// n small bitmaps, for thumbnails.
fn thumb_files(live: &live::LiveRoot, n: usize) -> Vec<PathBuf> {
    (0..n).map(|i| live.file(&format!("thumbs\\t{i:05}.bmp"), &bmp(i as u8))).collect()
}

/// Waits, at most `secs` from `since`, for the idle exit of host `pid` and prints how it went. The client is then
/// left as it is: dropping it would end a host the tester still watches.
fn idle_exit(c: ShellClient, pid: u32, since: Instant, secs: u64) {
    while since.elapsed() < Duration::from_secs(secs) {
        if c.state() == HostState::Down || matches!(c.state(), HostState::Ready { pid: p } if p != pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        let _ = c.drain();
    }
    let gone = matches!(c.state(), HostState::Down);
    println!(
        "probe: host {pid} {} after {} s idle",
        if gone { "exited" } else { "still runs" },
        since.elapsed().as_secs()
    );
    std::mem::forget(c);
}

fn menu(c: &ShellClient, file: &Path, at: (i32, i32)) {
    let Some(it) = item(file) else {
        println!("probe: bad --file");
        std::process::exit(2);
    };
    let t = Instant::now();
    let req = c.next_req();
    c.request(ShellReq::Menu { req, items: vec![it], at, extended: false, owner: 0 });
    let got = until(c, Duration::from_secs(60), ends(req));
    let shown = got.iter().any(|r| matches!(r, ShellReply::Evt(ShellEvt::MenuShown { .. })));
    match got.last() {
        Some(ShellReply::Evt(ShellEvt::MenuDone { outcome, .. })) if shown => {
            println!("probe: menu {outcome:?} after {} ms", t.elapsed().as_millis());
        }
        Some(ShellReply::Evt(ShellEvt::MenuDone { .. })) => println!("probe: no menu"),
        other => println!("probe: menu ended {other:?} after {} ms", t.elapsed().as_millis()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    let Some(root) = arg(&args, "--root").map(Path::new) else {
        println!("probe: --root <an existing %TEMP%\\mod-files-live-* folder> is required");
        std::process::exit(2);
    };
    let live = match live::LiveRoot::at(root) {
        Ok(l) => l,
        Err(e) => {
            println!("probe: {e}");
            std::process::exit(2);
        }
    };
    let exe = arg(&args, "--exe").map(PathBuf::from).unwrap_or_else(|| {
        let me = std::env::current_exe().unwrap_or_default();
        me.parent().and_then(Path::parent).map(|d| d.join("files.exe")).unwrap_or_default()
    });
    match mode {
        "menu-build" | "step3" => std::env::set_var("FILES_TEST_NO_UI", "1"),
        "menu-show" => std::env::set_var("FILES_TEST_MENU_DISMISS_MS", num(&args, "--dismiss-ms", 1_500).to_string()),
        _ => {}
    }
    let c = ShellClient::new(ShellConfig { exe, test: None }, Arc::new(|| {}));
    let pid = host_pid(&c);
    let px = num(&args, "--px", if mode == "thumbs" { 96 } else { 32 }).min(480) as u16;
    match mode {
        "icons" => {
            let (n, types) = (num(&args, "--n", 200) as usize, num(&args, "--types", 40) as usize);
            images(&c, &icon_files(&live, n, types), px, false);
        }
        "thumbs" => {
            images(&c, &thumb_files(&live, num(&args, "--n", 20) as usize), px, true);
        }
        "menu-build" | "menu-show" => {
            let file = arg(&args, "--file").map(PathBuf::from).unwrap_or_else(|| live.file("a.txt", b"x"));
            let mut at = windows::Win32::Foundation::POINT::default();
            if mode == "menu-show" {
                // SAFETY: a writable POINT.
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut at) };
            }
            menu(&c, &file, (at.x, at.y));
        }
        "idle" => {
            let req = c.next_req();
            c.request(ShellReq::ShellNewTypes { req });
            until(&c, Duration::from_secs(10), ends(req));
            idle_exit(c, pid, Instant::now(), num(&args, "--secs", 130));
        }
        "step3" => {
            // One host does the three, so its memory after them and its idle minute are those of one process.
            let (icons, thumbs) = (icon_files(&live, 200, 40), thumb_files(&live, 20));
            let file = arg(&args, "--file").map(PathBuf::from).unwrap_or_else(|| live.file("a.txt", b"x"));
            images(&c, &icons, 32, false);
            images(&c, &thumbs, 96, true);
            menu(&c, &file, (0, 0));
            println!("probe: idle from now");
            idle_exit(c, pid, Instant::now(), num(&args, "--secs", 130));
        }
        _ => {
            println!("probe: modes: icons, thumbs, menu-build, menu-show, idle, step3");
            std::process::exit(2);
        }
    }
}
