//! The budget rows the gallery owns (P2-kit K.CI1): the committed golden set's size, and the kit_min release exes that
//! CI measures (K.CI2) and that these tests check when the environment names them. The rows live in budgets.json.

use std::path::Path;

fn budgets() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("budgets.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("budgets.json parses")
}

/// The budgets.json row of that id.
fn row(id: &str) -> serde_json::Value {
    budgets()["rows"]
        .as_array()
        .expect("budgets.json has a rows array")
        .iter()
        .find(|r| r["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no budgets.json row {id}"))
}

/// The integer value (the maximum, in the row's unit) of a row.
fn max_of(id: &str) -> u64 {
    row(id)["value"].as_u64().unwrap_or_else(|| panic!("row {id} has no integer value"))
}

/// The bytes of every file under a folder; 0 when the folder does not exist.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
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

/// The size of the exe the environment variable names. A relative path is taken from the workspace root, where CI
/// builds the exes: cargo runs this test from the package's own folder.
fn exe_size(var: &str) -> u64 {
    let path = std::env::var(var).unwrap_or_else(|_| panic!("{var} must name a release exe"));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(path);
    std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).len()
}

#[test]
fn gallery_golden_set_fits_its_budget() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join("base");
    let bytes = dir_bytes(&base);
    assert!(bytes <= max_of("golden.gallery_bytes"), "the gallery golden set is {bytes} bytes, over its budget");
}

#[test]
#[ignore = "CI only: needs the kit_min release exe (KIT_MIN_EXE)"]
fn kit_min_size_within_row() {
    let id = std::env::var("KIT_MIN_ROW").unwrap_or_else(|_| "exe_size.kit_min".to_string());
    let size = exe_size("KIT_MIN_EXE");
    let max = max_of(&id);
    assert!(size <= max, "{id}: kit_min is {size} bytes, over its {max}");
}

#[test]
#[ignore = "CI only: needs the plain and the a11y kit_min release exes (KIT_MIN_EXE, KIT_MIN_A11Y_EXE)"]
fn kit_min_a11y_delta_within_350_kb() {
    const LIMIT: u64 = 358_400;
    let plain = exe_size("KIT_MIN_EXE");
    let a11y = exe_size("KIT_MIN_A11Y_EXE");
    let delta = a11y
        .checked_sub(plain)
        .unwrap_or_else(|| panic!("the a11y exe ({a11y} bytes) is smaller than the plain one ({plain} bytes)"));
    assert!(delta <= LIMIT, "the a11y feature adds {delta} bytes, over {LIMIT}");
}
