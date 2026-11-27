//! The benches (P2-kit K0.2): the timing helpers and the registry, complete in K0.2; each bench body lives in its own
//! file and returns None ("not ready") until its task fills it. `gallery --bench` runs them at budget x 1.0 (x 1.5 when
//! CI is set); tests/bench.rs and tests/bench_field.rs run the same bodies.

pub mod field;
pub mod raster;
pub mod table;

use std::time::Instant;

/// One bench: its name, its budgets.json row and its body (the median in ms, None while not ready).
pub struct Bench {
    pub name: &'static str,
    pub budget: &'static str,
    pub run: fn() -> Option<f64>,
}

static BENCHES: [Bench; 6] = [
    Bench { name: "raster_4k_settings_like", budget: "frame.raster_4k", run: raster::raster_4k_settings_like },
    Bench { name: "table_hover", budget: "frame.table_hover", run: table::table_hover },
    Bench { name: "table_text_40x4", budget: "frame.table_text_40x4", run: table::table_text_40x4 },
    Bench { name: "scroll_strip", budget: "frame.scroll_strip", run: table::scroll_strip },
    Bench { name: "table_4k_full", budget: "frame.table_4k_full", run: table::table_4k_full },
    Bench { name: "field_key", budget: "frame.field_key", run: field::field_key },
];

/// Every bench.
pub fn all() -> &'static [Bench] {
    &BENCHES
}

/// The bench of that name.
pub fn by_name(name: &str) -> Option<&'static Bench> {
    BENCHES.iter().find(|b| b.name == name)
}

/// The median of the samples (the mean of the two middle ones for an even count); NaN when empty.
pub fn median(samples: &mut [f64]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    }
}

/// Runs `f` `warmup` times, then times `runs` calls: the median in ms.
pub fn run(name: &str, warmup: u32, runs: u32, mut f: impl FnMut()) -> f64 {
    let _ = name;
    for _ in 0..warmup {
        f();
    }
    let mut samples: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    median(&mut samples)
}

/// The repository's budgets.json: CARGO_MANIFEST_DIR/../../budgets.json when built from the workspace, else beside the
/// exe.
fn budgets_path() -> Option<std::path::PathBuf> {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("budgets.json");
    if repo.is_file() {
        return Some(repo);
    }
    let beside = std::env::current_exe().ok()?.parent()?.join("budgets.json");
    beside.is_file().then_some(beside)
}

/// The value of a budgets.json row, in ms (a row in us is converted); NaN when the row or its value is missing.
pub fn budget_ms(id: &str) -> f64 {
    let Some(text) = budgets_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return f64::NAN;
    };
    budget_ms_in(&text, id)
}

/// budget_ms over the text of a budgets.json.
pub fn budget_ms_in(json: &str, id: &str) -> f64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return f64::NAN;
    };
    let row = v["rows"].as_array().and_then(|rows| rows.iter().find(|r| r["id"] == id));
    let Some(row) = row else {
        return f64::NAN;
    };
    let value = row["value"].as_f64().unwrap_or(f64::NAN);
    if row["unit"] == "us" {
        value / 1000.0
    } else {
        value
    }
}

/// Prints {"bench":..,"median_ms":..,"budget_ms":..} and returns false when the median is over the budget (x 1.5 when
/// CI is set, x 1.0 otherwise) or the budget row is missing.
pub fn gate(name: &str, id: &str, median: f64) -> bool {
    let budget = budget_ms(id);
    println!("{}", serde_json::json!({ "bench": name, "median_ms": median, "budget_ms": budget }));
    let allowance = if std::env::var_os("CI").is_some() { 1.5 } else { 1.0 };
    budget.is_finite() && median <= budget * allowance
}

/// The --bench mode: every ready bench (or the one named) through gate; 1 on a miss, a bench not ready or an unknown
/// name, so a run that measured nothing never passes.
pub fn run_all(filter: Option<&str>) -> i32 {
    let picked: Vec<&Bench> = BENCHES.iter().filter(|b| filter.is_none_or(|f| b.name == f)).collect();
    if picked.is_empty() {
        println!("no bench named {}", filter.unwrap_or_default());
        return 1;
    }
    let mut code = 0;
    for b in picked {
        match (b.run)() {
            None => {
                println!("not ready: {}", b.name);
                code = 1;
            }
            Some(median) => {
                if !gate(b.name, b.budget, median) {
                    code = 1;
                }
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_median_is_the_middle_value() {
        assert_eq!(median(&mut [5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert!(median(&mut []).is_nan());
        let mut calls = 0;
        let m = run("count", 2, 5, || calls += 1);
        assert_eq!(calls, 7);
        assert!(m >= 0.0);
    }

    #[test]
    fn budget_ms_reads_a_row() {
        // The repository's rows are read; their values belong to the budget owners, so only the inline JSON pins them.
        let full = budget_ms("frame.table_4k_full");
        assert!(full.is_finite() && full > 0.0, "{full}");
        assert!(budget_ms("frame.no_such_row").is_nan());
        let json = r#"{"rows":[{"id":"a.b","unit":"us","value":300},{"id":"c.d","unit":"ms"}]}"#;
        assert_eq!(budget_ms_in(json, "a.b"), 0.3);
        assert!(budget_ms_in(json, "c.d").is_nan());
        assert!(budget_ms_in("not json", "a.b").is_nan());
    }

    #[test]
    fn registry_names_and_rows() {
        let names: Vec<&str> = all().iter().map(|b| b.name).collect();
        assert_eq!(
            names,
            ["raster_4k_settings_like", "table_hover", "table_text_40x4", "scroll_strip", "table_4k_full", "field_key"]
        );
        assert!(all().iter().all(|b| b.budget.starts_with("frame.")));
        assert!(by_name("field_key").is_some() && by_name("nope").is_none());
        assert_eq!(run_all(Some("nope")), 1);
    }
}
