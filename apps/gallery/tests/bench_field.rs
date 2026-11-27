//! The field bench (K2b.3): its #[ignore] test, run by CI in release (`cargo test --release -p gallery --test bench_field -- --ignored`). A keystroke's edit, layout and repaint of a
//! field at 150 % takes a median of 0.3 ms at most (x 1.5 under CI). The budgets.json row frame.field_key is gated when it
//! exists (the CI group adds it); until then the plan's 0.3 ms stands.

use gallery::bench::{budget_ms, by_name, gate};

#[test]
#[ignore = "a release bench: cargo test --release -p gallery --test bench_field -- --ignored"]
fn field_key() {
    let b = by_name("field_key").expect("the field bench is registered");
    let median = (b.run)().expect("the field bench is ready");
    if budget_ms(b.budget).is_finite() {
        assert!(gate(b.name, b.budget, median), "bench field_key: median {median} ms is over its budget");
    } else {
        let allowance = if std::env::var_os("CI").is_some() { 1.5 } else { 1.0 };
        assert!(median <= 0.3 * allowance, "bench field_key: median {median} ms is over 0.3 ms");
    }
}
