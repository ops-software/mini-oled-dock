//! The kit benches (K2a.3, K2a.7): one #[ignore] test per bench of the registry, run by the CI kit bench job in release
//! (`cargo test --release -p gallery --test bench -- --ignored --nocapture`). Each runs the body of the registry's bench
//! (20 warm-up runs and 200 measured) and gates the median against its budgets.json row: x 1.5 under CI, x 1.0 elsewhere.

use gallery::bench::{by_name, gate};

fn check(name: &str) {
    let b = by_name(name).unwrap_or_else(|| panic!("no bench named {name}"));
    let median = (b.run)().unwrap_or_else(|| panic!("bench {name} is not ready"));
    assert!(gate(b.name, b.budget, median), "bench {name}: median {median} ms is over its budget");
}

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench -- --ignored"]
fn raster_4k_settings_like() {
    check("raster_4k_settings_like");
}

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench -- --ignored"]
fn table_hover() {
    check("table_hover");
}

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench -- --ignored"]
fn table_text_40x4() {
    check("table_text_40x4");
}

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench -- --ignored"]
fn scroll_strip() {
    check("scroll_strip");
}

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench -- --ignored"]
fn table_4k_full() {
    check("table_4k_full");
}
