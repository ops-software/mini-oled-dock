//! The test client's ops lines ("ops_*"): drive an OpsClient from a script, as the UI would, and print every event
//! as one JSON line on stdout for the test that started it. The client is made on the first line from this
//! process's FILES_* variables (its hooks: the host it starts gets the same ones).
//!
//! Lines: {"op":"ops_copy"|"ops_move","items":[path...],"dest":path,"conflicts":"ask"|"replace"|"skip"},
//! {"op":"ops_resume","of":n}, {"op":"ops_wait","ms":n} (until a Finished, at most ms).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use oled_trust::frame::files::{Conflicts, ItemRef, OpEvt, OpId, OpResult, WidePath};
use serde_json::{json, Value};

use crate::exit;
use crate::host::TestHooks;
use crate::ops_client::{OpsClient, OpsConfig, OpsEvent};

/// The hooks of this process: its FILES_* variables (the pipe and build tags apart).
fn hooks_from_env() -> TestHooks {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).map(Box::<str>::from);
    let env = std::env::vars()
        .filter(|(k, _)| k.starts_with("FILES_") && k != "FILES_TEST_PIPE_TAG" && k != "FILES_TEST_BUILD_TAG")
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    TestHooks { pipe_tag: var("FILES_TEST_PIPE_TAG"), build_tag: var("FILES_TEST_BUILD_TAG"), env }
}

fn client() -> &'static OpsClient {
    static CLIENT: OnceLock<OpsClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        let data_dir = std::env::var_os("FILES_DATA_DIR").map(PathBuf::from).unwrap_or_default();
        let cfg = OpsConfig { exe, data_dir, owner: 0, test: Some(hooks_from_env()) };
        OpsClient::new(cfg, Arc::new(|| {}))
    })
}

fn out(v: Value) {
    println!("{v}");
}

fn path(v: &Value) -> Option<WidePath> {
    v.as_str().and_then(|s| WidePath::parse(s).ok())
}

fn conflicts(v: &Value) -> Conflicts {
    match v.get("conflicts").and_then(Value::as_str) {
        Some("replace") => Conflicts::ReplaceAll,
        Some("skip") => Conflicts::SkipAll,
        _ => Conflicts::AskSystem,
    }
}

/// One event as the line the test reads.
pub fn event_line(e: &OpsEvent) -> Value {
    match e {
        OpsEvent::Evt(OpEvt::Accepted { op }) => json!({"evt": "accepted", "op": op.0}),
        OpsEvent::Evt(OpEvt::Started { op, items, .. }) => json!({"evt": "started", "op": op.0, "items": items}),
        OpsEvent::Evt(OpEvt::Progress { op, done, total, .. }) => {
            json!({"evt": "progress", "op": op.0, "done": done, "total": total})
        }
        OpsEvent::Evt(OpEvt::ItemsDone { op, changes }) => json!({"evt": "items_done", "op": op.0, "n": changes.len()}),
        OpsEvent::Evt(OpEvt::ItemFailed { op, item, hr }) => {
            json!({"evt": "item_failed", "op": op.0, "item": item.to_string_lossy(), "hr": hr})
        }
        OpsEvent::Evt(OpEvt::Finished { op, result, items, failed, .. }) => {
            let r = match result {
                OpResult::Done => "done",
                OpResult::Cancelled => "cancelled",
                OpResult::Failed => "failed",
                OpResult::Refused => "refused",
            };
            json!({"evt": "finished", "op": op.0, "result": r, "items": items, "failed": failed})
        }
        OpsEvent::Evt(OpEvt::Refused { op, why, .. }) => json!({"evt": "refused", "op": op.0, "why": *why as u8}),
        OpsEvent::Evt(OpEvt::Pending { of, done, total, .. }) => {
            json!({"evt": "pending", "of": of.0, "done": done, "total": total})
        }
        OpsEvent::Evt(OpEvt::NeedsInput { op, on }) => json!({"evt": "needs_input", "op": op.0, "on": on}),
        OpsEvent::Evt(e) => json!({"evt": "other", "debug": format!("{e:?}")}),
        OpsEvent::Undo(r) => json!({"evt": "undo", "of": r.of.0, "entries": r.entries.len()}),
        OpsEvent::HostLost => json!({"evt": "host_lost"}),
        OpsEvent::HostUpdated => json!({"evt": "host_updated"}),
    }
}

/// Runs one script line; 0 on success, else the exit code to end with.
pub fn run_line(line: &serde_json::Value) -> i32 {
    let op = line.get("op").and_then(Value::as_str).unwrap_or("");
    let c = client();
    let sent: Result<OpId, String> = match op {
        "ops_copy" | "ops_move" => {
            let items: Option<Vec<ItemRef>> = line
                .get("items")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| path(v).map(ItemRef::Path)).collect::<Option<Vec<_>>>())
                .unwrap_or(None);
            let (Some(items), Some(dest)) = (items, line.get("dest").and_then(path)) else { return exit::USAGE };
            let r = if op == "ops_copy" {
                c.copy(&items, &dest, conflicts(line))
            } else {
                c.move_to(&items, &dest, conflicts(line))
            };
            r.map_err(|e| format!("{e:?}"))
        }
        "ops_resume" => {
            let Some(of) = line.get("of").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()) else {
                return exit::USAGE;
            };
            c.resume_rest(OpId(of)).map_err(|e| format!("{e:?}"))
        }
        "ops_wait" => return wait(c, line.get("ms").and_then(Value::as_u64).unwrap_or(60_000).min(600_000)),
        _ => return exit::USAGE,
    };
    match sent {
        Ok(id) => {
            out(json!({"sent": id.0, "host_pid": c.host_pid()}));
            exit::OK
        }
        Err(e) => {
            out(json!({"error": e}));
            1
        }
    }
}

/// Prints events until a Finished (OK) or `ms` passed (1).
fn wait(c: &OpsClient, ms: u64) -> i32 {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        for e in c.drain() {
            out(event_line(&e));
            if matches!(e, OpsEvent::Evt(OpEvt::Finished { .. })) {
                return exit::OK;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    1
}
