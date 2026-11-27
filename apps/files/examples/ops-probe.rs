//! ops-probe: the live tester's tool for LIVE-F2 steps 1-2 (plan 5.2). Never run by a coding agent on the owner's
//! PC. It drives the ops host as the UI would and prints one JSON line per fact; every file it makes lives in a
//! LiveRoot (%TEMP%\mod-files-live-*), and user files change only through the host.
//!
//!   ops-probe attach --idle <secs> [--exe <files.exe>]
//!       starts (or reaches) the host, prints its pid, stays attached and idle, then says Bye.
//!   ops-probe copy --root <dir> --n <count> --size <bytes|NKiB|NMiB> [--kill-at <percent>] [--exe <files.exe>]
//!       makes <count> files in <dir>\src (LiveRoot::make_n), copies them to <dir>\dst through the host and prints
//!       progress, the host pid and whether every hash is equal; with --kill-at the probe ends itself at that
//!       percentage of the copy (the host finishes and writes ops-done.json).
//!   ops-probe verify --root <dir>
//!       prints whether every file of <dir>\src has an equal copy in <dir>\dst.
//!
//! A release host admits this probe only when files.exe was built with FILES_OPS_PROBE set in the build's environment
//! (host::named::OPS_PROBE_BUILD), with the probe beside it or in its examples folder:
//!   $env:FILES_OPS_PROBE='1'; cargo build -p files --release --locked --bins --examples
//! A files.exe built without it closes the pipe before its Hello, and the probe prints that hint.

#[path = "common/live.rs"]
mod live;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use files_engine::ops_client::{OpsClient, OpsConfig, OpsError, OpsEvent};
use live::LiveRoot;
use oled_trust::frame::files::{Conflicts, ItemRef, OpEvt, OpId, WidePath};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn say(v: serde_json::Value) {
    println!("{v}");
}

/// A client error, with the likely cause when the host never said Hello (a files.exe not built for the probe).
fn fail(e: &OpsError) {
    let hint = matches!(e, OpsError::Unavailable("no Hello from the ops host"))
        .then_some("the host admits this probe only when files.exe was built with FILES_OPS_PROBE set");
    say(serde_json::json!({"error": format!("{e:?}"), "hint": hint}));
}

/// --exe, else files.exe next to the probe or one folder up (target\<profile>\examples\ops-probe.exe).
fn files_exe(args: &[String]) -> Option<PathBuf> {
    if let Some(e) = arg(args, "--exe") {
        return Some(PathBuf::from(e));
    }
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    [dir.join("files.exe"), dir.parent()?.join("files.exe")].into_iter().find(|p| p.is_file())
}

fn client(args: &[String]) -> Option<OpsClient> {
    let exe = files_exe(args)?;
    let data_dir = files_engine::own_files::OwnDir::default_dir()?.path().to_path_buf();
    Some(OpsClient::new(OpsConfig { exe, data_dir, owner: 0, test: None }, Arc::new(|| {})))
}

/// "4MiB", "512KiB" or a plain byte count.
fn size(s: &str) -> Option<u64> {
    let (n, mul) = if let Some(n) = s.strip_suffix("MiB") {
        (n, 1 << 20)
    } else if let Some(n) = s.strip_suffix("KiB") {
        (n, 1 << 10)
    } else {
        (s, 1)
    };
    n.parse::<u64>().ok().map(|n| n * mul)
}

fn wide(p: &Path) -> Option<WidePath> {
    WidePath::parse(&p.display().to_string()).ok()
}

fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("attach") => attach(args),
        Some("copy") => copy(args),
        Some("verify") => verify(args),
        _ => {
            eprintln!("usage: ops-probe attach --idle <secs> | copy --root <dir> --n <count> --size <size> [--kill-at <pct>] | verify --root <dir>");
            2
        }
    }
}

fn attach(args: &[String]) -> i32 {
    let idle: u64 = arg(args, "--idle").and_then(|s| s.parse().ok()).unwrap_or(60);
    let Some(c) = client(args) else { return 2 };
    // A request no host knows (resume of operation 0): it starts or reaches the host and attaches, changing nothing.
    if let Err(e) = c.resume_rest(OpId(0)) {
        fail(&e);
        return 1;
    }
    say(serde_json::json!({"host_pid": c.host_pid(), "idle_s": idle}));
    std::thread::sleep(Duration::from_secs(idle));
    let _ = c.drain();
    c.shutdown();
    say(serde_json::json!({"detached": true}));
    0
}

fn copy(args: &[String]) -> i32 {
    let (Some(root), Some(n), Some(bytes)) =
        (arg(args, "--root"), arg(args, "--n").and_then(|s| s.parse::<u32>().ok()), arg(args, "--size").and_then(size))
    else {
        return 2;
    };
    let kill_at: Option<u64> = arg(args, "--kill-at").and_then(|s| s.parse().ok());
    let root = match LiveRoot::at(Path::new(root)) {
        Ok(r) => r,
        Err(e) => {
            say(serde_json::json!({"error": e}));
            return 2;
        }
    };
    let made = root.make_n("src", n, bytes);
    let dst = root.dir("dst");
    let (Some(items), Some(dest)) =
        (made.iter().map(|p| wide(p).map(ItemRef::Path)).collect::<Option<Vec<_>>>(), wide(&dst))
    else {
        return 2;
    };
    let Some(c) = client(args) else { return 2 };
    let start = Instant::now();
    let op = match c.copy(&items, &dest, Conflicts::AskSystem) {
        Ok(op) => op,
        Err(e) => {
            fail(&e);
            return 1;
        }
    };
    say(serde_json::json!({"op": op.0, "host_pid": c.host_pid(), "files": n, "bytes_each": bytes}));
    loop {
        for e in c.drain() {
            match e {
                OpsEvent::Evt(OpEvt::Progress { done, total, bps, .. }) => {
                    say(
                        serde_json::json!({"progress": done, "total": total, "bps": bps, "ms": start.elapsed().as_millis() as u64}),
                    );
                    if let Some(pct) = kill_at {
                        if total > 0 && done * 100 >= total * pct {
                            say(serde_json::json!({"killed_at_percent": pct}));
                            // The probe plays a UI that dies mid-copy: no Bye, no cleanup, no crash report.
                            std::process::exit(9);
                        }
                    }
                }
                OpsEvent::Evt(OpEvt::Finished { op: o, result, items, failed, .. }) if o == op => {
                    let equal = hashes_equal(&root, n);
                    say(
                        serde_json::json!({"finished": format!("{result:?}"), "items": items, "failed": failed, "hashes_equal": equal, "ms": start.elapsed().as_millis() as u64}),
                    );
                    c.shutdown();
                    return if equal { 0 } else { 1 };
                }
                OpsEvent::HostLost => {
                    say(serde_json::json!({"host_lost": true}));
                    return 1;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hashes_equal(root: &LiveRoot, n: u32) -> bool {
    (1..=n).all(|i| {
        let name = format!("f{i:06}.bin");
        root.at_rel(&format!("dst/{name}")).is_file()
            && root.hash(&format!("dst/{name}")) == root.hash(&format!("src/{name}"))
    })
}

fn verify(args: &[String]) -> i32 {
    let Some(root) = arg(args, "--root").and_then(|r| LiveRoot::at(Path::new(r)).ok()) else { return 2 };
    let n = std::fs::read_dir(root.at_rel("src")).map(|d| d.count() as u32).unwrap_or(0);
    let equal = n > 0 && hashes_equal(&root, n);
    say(serde_json::json!({"files": n, "hashes_equal": equal}));
    if equal {
        0
    } else {
        1
    }
}
