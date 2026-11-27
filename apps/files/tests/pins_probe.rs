//! The pins probe of spike FS-2 (plan F4.7): its argument table and the promise that it changes nothing in Explorer.
//! The probe's source is included as a module, so the pure parts run here without any shell call.
#![allow(dead_code)]

#[path = "../examples/pins-probe.rs"]
mod probe;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn probe_args_table() {
    let ok = probe::parse_args(&v(&["list", "--json", "out.json"])).expect("accepted");
    assert_eq!(ok.out, std::path::PathBuf::from("out.json"));
    for bad in [
        v(&[]),
        v(&["list"]),
        v(&["list", "--json"]),
        v(&["list", "--json", ""]),
        v(&["list", "--json", "a", "b"]),
        v(&["list", "--out", "a"]),
        v(&["pin", "--json", "a"]),
        v(&["--json", "a", "list"]),
        v(&["LIST", "--json", "a"]),
    ] {
        assert!(probe::parse_args(&bad).is_err(), "{bad:?} must be refused (exit 2)");
    }
}

#[test]
fn probe_writes_nothing_but_out() {
    let src = include_str!("../examples/pins-probe.rs");
    // The banned names are built in pieces so this file never trips a scan for them.
    let banned = [
        concat!("Set", "Property"),
        concat!("Set", "Value"),
        concat!("Set", "Bool"),
        concat!("P", "in", "To"),
        concat!("Un", "pin"),
        concat!("Invoke", "Verb"),
        concat!("Invoke", "Command"),
        concat!("Reg", "Set"),
        concat!("Reg", "Create"),
        concat!("Reg", "Delete"),
        concat!("Reg", "Open"),
        concat!("Win32_System_", "Registry"),
        concat!("Move", "File"),
        concat!("Delete", "File"),
        concat!("fs", "::write"),
        concat!("S", "et"),
        concat!("P", "in"),
    ];
    let banned_ci = [concat!("pin", "tohome"), concat!("unpin", "fromhome"), concat!("Shell", "Execute")];
    for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
        for b in banned {
            assert!(!line.contains(b), "the probe names {b}: {line}");
        }
        let low = line.to_lowercase();
        for b in banned_ci {
            assert!(!low.contains(&b.to_lowercase()), "the probe names {b}: {line}");
        }
    }
    // Its one write goes through the LiveRoot fence.
    assert!(
        src.lines().filter(|l| !l.trim_start().starts_with("//")).any(|l| l.contains("LiveRoot::at(")),
        "the probe's write must go through LiveRoot::at(..)"
    );
}
