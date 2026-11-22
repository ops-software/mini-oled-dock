//! Reader and checks of budgets.json (ROADMAP 6.2, SPEC-platform 5.7.4).
//! Pure functions over the file; the only gate that needs a build is the ignored `exe_size_within_ledger`.

use std::path::{Path, PathBuf};

use serde_json::Value;

const MIB: f64 = 1_048_576.0;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn budgets_text() -> String {
    std::fs::read_to_string(root().join("budgets.json")).expect("budgets.json at the repository root")
}

fn budgets() -> Value {
    serde_json::from_str(&budgets_text()).expect("budgets.json is valid JSON")
}

fn rows() -> Vec<Value> {
    budgets()["rows"].as_array().expect("rows is an array").clone()
}

fn row(id: &str) -> Value {
    let mut hit = rows().into_iter().filter(|r| r["id"] == id);
    let r = hit.next().unwrap_or_else(|| panic!("budgets.json has no row {id}"));
    assert!(hit.next().is_none(), "budgets.json has the row {id} twice");
    r
}

fn value(id: &str) -> f64 {
    row(id)["value"].as_f64().unwrap_or_else(|| panic!("row {id} has no numeric value"))
}

/// "1.4.3" -> (1, 4, 3). Missing parts count as 0; anything that is not a number is an error.
fn semver(s: &str) -> Result<(u64, u64, u64), String> {
    let mut it = s.split('.');
    let mut part = || -> Result<u64, String> {
        match it.next() {
            None => Ok(0),
            Some(p) => p.parse::<u64>().map_err(|_| format!("bad version {s:?}")),
        }
    };
    let v = (part()?, part()?, part()?);
    if it.next().is_some() {
        return Err(format!("bad version {s:?}"));
    }
    Ok(v)
}

/// The cumulative allowance table of the exe_size.dock row, in file order: (version, KiB).
fn ledger() -> Vec<(String, u64)> {
    row("exe_size.dock")["ledger_kib_cumulative"]
        .as_object()
        .expect("ledger_kib_cumulative is an object")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().expect("ledger values are whole KiB")))
        .collect()
}

/// Size limit in bytes for release `target`: the measured 1.4.2 base plus the cumulative allowance.
fn limit(base_bytes: u64, target: &str, ledger: &[(String, u64)]) -> Result<u64, String> {
    ledger
        .iter()
        .find(|(v, _)| v == target)
        .map(|(_, kib)| base_bytes + kib * 1024)
        .ok_or_else(|| format!("target {target:?} is not in the ledger"))
}

fn limit_of_budgets_json() -> u64 {
    let r = row("exe_size.dock");
    let base = r["base_bytes"].as_u64().expect("base_bytes");
    limit(base, r["target"].as_str().expect("target"), &ledger()).unwrap()
}

fn cargo_version() -> String {
    let toml = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    let pkg = toml.split("[package]").nth(1).expect("[package] in Cargo.toml");
    let line = pkg
        .lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .find(|l| l.trim_start().starts_with("version"))
        .expect("version in [package]");
    line.split('"').nth(1).expect("quoted version").to_string()
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_bytes(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[test]
fn schema_is_valid() {
    let b = budgets();
    assert_eq!(b["schema"], 1);
    let rows = rows();
    let mut ids = std::collections::HashSet::new();
    for r in &rows {
        let id = r["id"].as_str().expect("every row has an id");
        assert!(ids.insert(id.to_string()), "duplicate id {id}");
        let one_of = |key: &str, set: &[&str]| {
            let v = r[key].as_str().unwrap_or_else(|| panic!("{id}: {key} missing"));
            assert!(set.contains(&v), "{id}: {key} = {v:?} not in {set:?}");
            v.to_string()
        };
        let wh = one_of("where", &["CI", "VM", "OWNER"]);
        let st = one_of("status", &["active", "pending"]);
        one_of("unit", &["bytes", "count", "ms", "ctxsw_per_s", "percent_core"]);
        let gate = one_of("gate", &["max", "ledger", "file", "baseline_plus_15pct"]);
        if gate == "max" {
            assert!(r["value"].is_number(), "{id}: a max row needs a numeric value");
        }
        if st == "active" {
            assert_eq!(wh, "CI", "{id}: an active row must run in CI");
        }
        if r.get("provisional").is_some() {
            let ok = [
                "private.kit_",
                "private.tasks_",
                "private.files_",
                "private.uia_",
                "exe_size.tasks",
                "exe_size.files",
            ]
            .iter()
            .any(|p| id.starts_with(p));
            assert!(ok, "{id}: provisional is only for the kit/tasks/files/tasks_admin/uia rows");
        }
        if gate == "file" {
            let f = r["file"].as_str().expect("a file row names its file");
            let n: u64 = std::fs::read_to_string(root().join(f))
                .unwrap_or_else(|e| panic!("{id}: {f}: {e}"))
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("{id}: {f} is not a number"));
            assert!(n > 0, "{id}: {f} is zero");
        }
    }
    // the ledger row: target present in the ledger, keys in ascending version order (file order)
    let l = row("exe_size.dock");
    assert_eq!(l["gate"], "ledger");
    let target = l["target"].as_str().unwrap();
    let lg = ledger();
    assert!(lg.iter().any(|(v, _)| v == target), "target {target} is not in the ledger");
    let text = budgets_text();
    let start = text.find("ledger_kib_cumulative").unwrap();
    let mut last = (0, 0, 0);
    let mut last_pos = start;
    for (v, _) in &lg {
        let pos = start + text[start..].find(&format!("\"{v}\"")).expect("ledger key in the file");
        assert!(pos >= last_pos, "ledger keys are not in file order");
        last_pos = pos;
        let s = semver(v).unwrap();
        assert!(s >= last, "ledger keys are not ascending at {v}");
        last = s;
    }
    let kibs: Vec<u64> = lg.iter().map(|(_, k)| *k).collect();
    assert!(kibs.windows(2).all(|w| w[0] <= w[1]), "the cumulative allowance must not fall");
}

#[test]
fn active_ledger_base_is_measured() {
    let r = row("exe_size.dock");
    if r["status"] == "active" {
        assert!(r["base_bytes"].as_u64().unwrap_or(0) > 0, "an active ledger row needs a measured base_bytes");
    }
}

#[test]
fn ledger_target_is_not_behind_cargo() {
    let target = row("exe_size.dock")["target"].as_str().unwrap().to_string();
    let cargo = cargo_version();
    assert!(
        semver(&target).unwrap() >= semver(&cargo).unwrap(),
        "Cargo.toml says {cargo} but the ledger target is {target}: bump `target` in budgets.json with the version"
    );
}

#[test]
fn g7_values_are_pinned() {
    // Changing one needs this test edited too, which is a change the owner reviews.
    assert_eq!(value("private.dock_hidden"), 12.0 * MIB);
    assert_eq!(value("private.kit_empty"), 4.0 * MIB);
    assert_eq!(value("private.tasks_640x560"), 15.0 * MIB);
    assert_eq!(value("private.tasks_3840x2160"), 15.0 * MIB);
    assert_eq!(value("private.files_1100x720"), 20.0 * MIB);
    assert_eq!(value("private.files_3840x2160"), 20.0 * MIB);
    assert_eq!(value("private.tasks_admin"), 4.0 * MIB);
    assert_eq!(value("private.uia_client_delta"), 2.0 * MIB);
    assert_eq!(value("leak.open_close_20"), MIB);
    assert_eq!(value("wakeups.dock_hidden"), 0.25);
    assert_eq!(value("wakeups.kit_static"), 0.0);
    assert_eq!(value("timers.dock_hidden"), 0.0);
    assert_eq!(value("frame.table_hover"), 0.5);
    assert_eq!(value("frame.table_text_40x4"), 1.5);
    assert_eq!(value("frame.scroll_strip"), 2.0);
    assert_eq!(value("frame.table_4k_full"), 8.0);
    assert_eq!(value("frame.screen_p95_4k"), 8.0);
    assert_eq!(value("frame.tears_per_100"), 1.0);
    assert_eq!(value("start.first_frame_tasks"), 100.0);
    assert_eq!(value("start.first_frame_files"), 100.0);
    assert_eq!(value("files.list_10k"), 50.0);
    assert_eq!(value("tasks.cpu_1s"), 0.5);
    assert_eq!(value("exe_size.tasks"), (1.8 * MIB).round());
    assert_eq!(value("exe_size.files"), 2.5 * MIB);
    assert_eq!(value("exe_size.tasks_admin"), (0.6 * MIB).round());
    assert_eq!(value("golden.bytes"), 32.0 * MIB);
}

#[test]
fn every_g7_budget_has_a_row() {
    // The list is the contract: each id exists exactly once, so deleting a row fails here.
    const IDS: &[&str] = &[
        "exe_size.dock",
        "golden.bytes",
        "tests.floor",
        "private.dock_hidden",
        "wakeups.dock_hidden",
        "timers.dock_hidden",
        "wakeups.kit_static",
        "private.kit_empty",
        "private.tasks_640x560",
        "private.tasks_3840x2160",
        "private.files_1100x720",
        "private.files_3840x2160",
        "private.tasks_admin",
        "private.uia_client_delta",
        "leak.open_close_20",
        "exe_size.tasks",
        "exe_size.files",
        "exe_size.tasks_admin",
        "frame.bar_150",
        "frame.table_hover",
        "frame.table_text_40x4",
        "frame.scroll_strip",
        "frame.table_4k_full",
        "frame.screen_p95_4k",
        "frame.tears_per_100",
        "start.first_frame_tasks",
        "start.first_frame_files",
        "files.list_10k",
        "tasks.cpu_1s",
    ];
    let rows = rows();
    for id in IDS {
        assert_eq!(rows.iter().filter(|r| r["id"] == *id).count(), 1, "row {id} must exist exactly once");
    }
    assert_eq!(rows.len(), IDS.len(), "a row is not in the contract list (add it here and to g7_values_are_pinned)");
}

#[test]
fn ledger_limit_arithmetic() {
    let l = vec![("1.4.2".to_string(), 0), ("1.4.3".to_string(), 80), ("1.4.4".to_string(), 120)];
    assert_eq!(limit(1_400_000, "1.4.3", &l).unwrap(), 1_400_000 + 80 * 1024);
    assert_eq!(limit(1_400_000, "1.4.2", &l).unwrap(), 1_400_000);
    assert!(limit(1_400_000, "9.9.9", &l).is_err());
    // and on the real file
    assert!(limit_of_budgets_json() > row("exe_size.dock")["base_bytes"].as_u64().unwrap());
}

#[test]
fn semver_compares_numerically() {
    assert!(semver("1.10.0").unwrap() > semver("1.9.0").unwrap());
    assert!(semver("1.4.3").unwrap() > semver("1.4.2").unwrap());
    assert!(semver("x.y").is_err());
}

#[test]
#[ignore = "needs a release build: DOCK_EXE=<path>"]
fn exe_size_within_ledger() {
    let size = std::fs::metadata(std::env::var("DOCK_EXE").expect("DOCK_EXE")).unwrap().len();
    assert!(size <= limit_of_budgets_json(), "dock.exe is {size} bytes");
}

#[test]
fn golden_set_fits_its_budget() {
    // Only the committed set counts; an untracked tests/golden/local never does.
    let used = dir_bytes(&root().join("tests/golden/base"));
    let max = value("golden.bytes") as u64;
    assert!(used <= max, "tests/golden/base is {used} bytes, budget {max}");
}
