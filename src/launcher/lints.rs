//! Source lints of the launcher (LC-2). Each lint is a pure `violations` function over (path, text), tested on a good and a bad
//! snippet first and then run over the real tree. A lint whose subject file does not exist yet passes.
//! This file holds the forbidden words as fixtures, so every tree scan skips it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;

const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

const DENY: &str = "#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]";
const OWNER_PROBE_CFG: &str = "#[cfg(feature = \"owner-probe\")]";
/// The four launcher files that call Windows (lints 5 and 6).
const WIN32_FILES: [&str; 4] =
    ["src/popup_launcher.rs", "src/launcher_sys.rs", "src/launcher_files.rs", "src/launcher_probe.rs"];

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Byte offsets of `token` with a word boundary on both sides (so `Verb::RunAs` does not match `Verb::RunAsAdmin`
/// nor `ShellVerb::RunAs`).
fn tokens(text: &str, token: &str) -> Vec<usize> {
    text.match_indices(token)
        .filter(|(i, _)| {
            let before = text.get(..*i).and_then(|t| t.chars().next_back());
            let after = text.get(i + token.len()..).and_then(|t| t.chars().next());
            !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Offset of the first `#[cfg(test)]` (everything after it counts as test code); the text length when there is none.
fn test_start(text: &str) -> usize {
    text.find("#[cfg(test)]").unwrap_or(text.len())
}

fn line_of(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |t| t.matches('\n').count()) + 1
}

/// The body (without the outer braces) of the first `fn name(` found at or after `from`: (open brace + 1, close brace).
fn fn_body(text: &str, sig: &str, from: usize) -> Option<(usize, usize)> {
    let start = from + text.get(from..)?.find(sig)?;
    let open = start + text.get(start..)?.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut prev = ' ';
    let mut in_comment = false;
    let mut it = text.get(open..)?.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if in_comment {
            in_comment = c != '\n';
            continue;
        }
        if in_str {
            if c == '"' && prev != '\\' {
                in_str = false;
            }
            prev = if prev == '\\' && c == '\\' { ' ' } else { c };
            continue;
        }
        if c == '/' && !matches!(prev, '\'') && it.peek().is_some_and(|&(_, n)| n == '/') {
            in_comment = true;
            continue;
        }
        match c {
            '"' if prev != '\'' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, open + i));
                }
            }
            _ => {}
        }
        prev = c;
    }
    None
}

/// `body` without its comment lines: a comment never satisfies a lint.
fn code_of(body: &str) -> String {
    body.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

// ---- lint 1: who may build a run-as request --------------------------------------------------------------------------

fn runas_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    let tests = test_start(text);
    let mut rule = |token: &str, allowed: &[&str], test_only_in: &[&str]| {
        for at in tokens(text, token) {
            let ok = allowed.contains(&path) || (test_only_in.contains(&path) && at > tests);
            if !ok {
                v.push(format!("{path}:{}: {token} is not allowed here", line_of(text, at)));
            }
        }
    };
    rule("Gesture::from_key", &["src/launcher/gesture.rs", "src/popup_launcher.rs"], &["src/hook.rs"]);
    rule("Gesture::for_test", &[], &[path]);
    rule("ShellRequest::RunAsTyped", &["src/shell_thread.rs", "src/launcher_sys.rs"], &[]);
    rule("LaunchReq::RunAsTyped", &["src/launcher/session.rs", "src/launcher_sys.rs"], &[]);
    rule("Verb::RunAs", &["src/shell_thread.rs"], &[]);
    if path == "src/shell_thread.rs" {
        let n = tokens(text, "Verb::RunAs").len();
        if n > 4 {
            v.push(format!("{path}: Verb::RunAs appears {n} times, at most 4 are allowed"));
        }
    }
    v
}

#[test]
fn runas_is_built_only_where_allowed() {
    let bad = "fn f() { let g = Gesture::from_key(1); }";
    assert_eq!(runas_violations("src/ui.rs", bad).len(), 1);
    assert!(runas_violations("src/launcher/gesture.rs", bad).is_empty());
    assert!(runas_violations("src/popup_launcher.rs", bad).is_empty());
    // hook.rs: only after the first #[cfg(test)]
    assert_eq!(runas_violations("src/hook.rs", bad).len(), 1);
    let hook = "fn f() {}\n#[cfg(test)]\nmod t { fn g() { Gesture::from_key(2); } }";
    assert!(runas_violations("src/hook.rs", hook).is_empty());
    // for_test: only in test code of its own file
    assert_eq!(runas_violations("src/launcher/gesture.rs", "fn f() { Gesture::for_test(1); }").len(), 1);
    assert!(runas_violations("src/launcher/gesture.rs", "#[cfg(test)]\nfn f() { Gesture::for_test(1); }").is_empty());
    // request kinds
    let st = "ShellRequest::RunAsTyped { x }";
    assert_eq!(runas_violations("src/ui.rs", st).len(), 1);
    assert!(runas_violations("src/shell_thread.rs", st).is_empty());
    assert!(runas_violations("src/launcher_sys.rs", st).is_empty());
    let lr = "LaunchReq::RunAsTyped { x }";
    assert_eq!(runas_violations("src/launcher/providers.rs", lr).len(), 1);
    assert!(runas_violations("src/launcher/session.rs", lr).is_empty());
    assert!(runas_violations("src/launcher_sys.rs", lr).is_empty());
    // Verb::RunAs: shell_thread only, at most 4; word boundary
    assert_eq!(runas_violations("src/ui.rs", "Verb::RunAs").len(), 1);
    assert!(runas_violations("src/ui.rs", "Verb::RunAsAdmin").is_empty());
    assert!(runas_violations("src/ui.rs", "ShellVerb::RunAs").is_empty());
    assert!(runas_violations("src/shell_thread.rs", &"Verb::RunAs\n".repeat(4)).is_empty());
    assert_eq!(runas_violations("src/shell_thread.rs", &"Verb::RunAs\n".repeat(5)).len(), 1);
    assert!(runas_violations("src/shell_thread.rs", &"Verb::RunAsAdmin\n".repeat(9)).is_empty());
    // the real tree
    let all = scan_tree(runas_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 2: launcher logs carry no user text ------------------------------------------------------------------------

const LOG_OK: [&str; 9] = ["n", "len", "count", "ms", "us", "gen", "seq", "kind:?", "id:?"];
const LOG_OK_MORE: [&str; 2] = ["bytes", "e"];
const LOG_OK_PROBE: [&str; 3] = ["image", "t", "m"];

fn in_logged_files(path: &str) -> bool {
    (path.starts_with("src/launcher/") && path != "src/launcher/lints.rs" && path != "src/launcher/testkit.rs")
        || path == "src/popup_launcher.rs"
        || path == "src/launcher_sys.rs"
        || path == "src/launcher_files.rs"
        || path == "src/launcher_probe.rs"
}

/// Every `log!(` call: (offset of the call, its text up to the matching parenthesis).
fn log_calls(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("log!(") {
        if text.get(..i).and_then(|t| t.chars().next_back()).is_some_and(is_ident) {
            continue;
        }
        let open = i + 4;
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut end = text.len();
        for (k, c) in text.get(open..).unwrap_or("").char_indices() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((i, text.get(i..end).unwrap_or("")));
    }
    out
}

/// The first string literal of a call (the format string) and its placeholders; None when the call has no literal first.
fn placeholders(call: &str) -> Option<Vec<String>> {
    let rest = call.strip_prefix("log!(")?.trim_start();
    let mut chars = rest.strip_prefix('"')?.chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return Some(out),
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    continue;
                }
                let mut name = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    name.push(d);
                }
                out.push(name);
            }
            _ => {}
        }
    }
    None
}

fn log_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if in_logged_files(path) {
        let probe = path == "src/launcher_probe.rs";
        for (at, call) in log_calls(text) {
            match placeholders(call) {
                None => v.push(format!("{path}:{}: log! without a literal format string", line_of(text, at))),
                Some(ph) => {
                    for p in ph {
                        let ok = LOG_OK.contains(&p.as_str())
                            || LOG_OK_MORE.contains(&p.as_str())
                            || (probe && LOG_OK_PROBE.contains(&p.as_str()));
                        if !ok {
                            v.push(format!(
                                "{path}:{}: log! placeholder {{{p}}} may carry user text",
                                line_of(text, at)
                            ));
                        }
                    }
                }
            }
        }
    }
    if path == "src/shell_thread.rs" {
        let begin = "// launcher requests: begin";
        let end = "// launcher requests: end";
        let (b, e) = (text.find(begin), text.find(end));
        if b.is_some() != e.is_some() || b.zip(e).is_some_and(|(b, e)| e < b) {
            v.push(format!("{path}: launcher requests markers must both be present, begin before end"));
        }
        if let Some((b, e)) = b.zip(e).filter(|(b, e)| b <= e) {
            let region = text.get(b..e).unwrap_or("");
            for (at, call) in log_calls(region) {
                let ph = placeholders(call).unwrap_or_default();
                let counted = call.contains("({} chars)") && call.contains(".chars().count()") && ph.len() == 1;
                let err_only = !ph.is_empty() && ph.iter().all(|p| p == "e");
                if !counted && !err_only {
                    v.push(format!(
                        "{path}:{}: launcher request log must be log!(\"shell: <Kind> ({{}} chars) ...\", x.chars().count())",
                        line_of(text, b + at)
                    ));
                }
            }
        }
    }
    v
}

#[test]
fn launcher_logs_carry_no_text() {
    let good = "fn f() { log!(\"launcher: {n} rows in {ms} ms, {kind:?} {{braces}}\", a, b); }";
    assert!(log_violations("src/launcher/session.rs", good).is_empty());
    assert!(log_violations("src/launcher_sys.rs", "log!(\"launcher: failed {e}\");").is_empty());
    assert_eq!(log_violations("src/launcher/session.rs", "log!(\"query {}\", q);").len(), 1);
    assert_eq!(log_violations("src/popup_launcher.rs", "log!(\"query {text}\");").len(), 1);
    assert_eq!(log_violations("src/launcher_files.rs", "log!(\"a {len} b {path:?}\");").len(), 1);
    // other files are not in scope
    assert!(log_violations("src/ui.rs", "log!(\"query {}\", q);").is_empty());
    // the probe may also name an image and tick times, nobody else
    let probe = "log!(\"probe: {image} {t} {m}\");";
    assert!(log_violations("src/launcher_probe.rs", probe).is_empty());
    assert_eq!(log_violations("src/launcher_sys.rs", probe).len(), 3);
    // an identifier ending in log is not the macro
    assert!(log_violations("src/launcher/session.rs", "blog!(\"{}\", x);").is_empty());
    // shell_thread region
    let st =
        |body: &str| format!("a\n// launcher requests: begin\n{body}\n// launcher requests: end\nlog!(\"x {{}}\", 1);");
    assert!(log_violations("src/shell_thread.rs", &st("log!(\"shell: Run ({} chars) start\", s.chars().count());"))
        .is_empty());
    assert!(log_violations("src/shell_thread.rs", &st("log!(\"shell: Run failed {e}\");")).is_empty());
    assert_eq!(log_violations("src/shell_thread.rs", &st("log!(\"shell: Run {}\", s);")).len(), 1);
    assert_eq!(
        log_violations("src/shell_thread.rs", &st("log!(\"shell: Run ({} chars) {}\", s.chars().count(), s);")).len(),
        1
    );
    // a missing, misspelled or swapped marker fails instead of skipping the check
    let half = "a\n// launcher requests: begin\nlog!(\"shell: Run {}\", s);\n";
    assert_eq!(log_violations("src/shell_thread.rs", half).len(), 1);
    let swapped = "// launcher requests: end\n// launcher requests: begin\n";
    assert_eq!(log_violations("src/shell_thread.rs", swapped).len(), 1);
    let all = scan_tree(log_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 3: the pure modules do not reach Windows -----------------------------------------------------------------

const REACH: [&str; 8] = [
    "windows::",
    "std::fs",
    "std::net",
    "std::process",
    "CreateFile",
    "FindFirstFile",
    "GetFileAttributes",
    "std::env",
];

fn reach_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if !path.starts_with("src/launcher/") || path == "src/launcher/lints.rs" || path == "src/launcher/testkit.rs" {
        return v;
    }
    for word in REACH {
        for (at, _) in text.match_indices(word) {
            v.push(format!("{path}:{}: {word} in a pure module", line_of(text, at)));
        }
    }
    for (at, _) in text.match_indices("Reg") {
        let before = text.get(..at).and_then(|t| t.chars().next_back());
        let after = text.get(at + 3..).and_then(|t| t.chars().next());
        if !before.is_some_and(|c| c.is_ascii_alphanumeric()) && after.is_some_and(|c| c.is_ascii_uppercase()) {
            v.push(format!("{path}:{}: registry call in a pure module", line_of(text, at)));
        }
    }
    v
}

#[test]
fn pure_modules_do_not_reach_windows() {
    assert_eq!(reach_violations("src/launcher/rank.rs", "use windows::Win32::Foundation::HWND;").len(), 1);
    assert_eq!(reach_violations("src/launcher/rank.rs", "let s = std::fs::read(p);").len(), 1);
    assert_eq!(reach_violations("src/launcher/rank.rs", "RegGetValueW(a);").len(), 1);
    assert_eq!(reach_violations("src/launcher/rank.rs", "let a = std::env::var(x); CreateFileW(p);").len(), 2);
    assert!(reach_violations("src/launcher/rank.rs", "let regex = Regex::new(); let r = Region; ProviderRegistry;")
        .is_empty());
    assert!(reach_violations("src/launcher/lints.rs", "windows:: std::fs").is_empty());
    assert!(reach_violations("src/launcher/testkit.rs", "std::env").is_empty());
    assert!(reach_violations("src/popup_launcher.rs", "windows:: std::fs").is_empty());
    let all = scan_tree(reach_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 4: probe switches live only in owner builds --------------------------------------------------------------

fn probe_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path == "src/launcher/lints.rs" {
        return v;
    }
    if path != "src/launcher_probe.rs" {
        for (at, _) in text.match_indices("DOCK_LAUNCHER_") {
            // A switch name starts at a word boundary: WM_DOCK_LAUNCHER_YIELDED is a window message, not a switch.
            if text.get(..at).and_then(|t| t.chars().next_back()).is_some_and(is_ident) {
                continue;
            }
            v.push(format!("{path}:{}: DOCK_LAUNCHER_ switch outside launcher_probe.rs", line_of(text, at)));
        }
        if path != "src/hook.rs" {
            for (at, _) in text.match_indices("DOCK_TEST_FREEZE_UI_MS") {
                v.push(format!(
                    "{path}:{}: DOCK_TEST_FREEZE_UI_MS outside hook.rs and launcher_probe.rs",
                    line_of(text, at)
                ));
            }
        }
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            let names_mod = path == "src/main.rs" && l.trim() == "mod launcher_probe;";
            if names_mod || l.contains("launcher_probe::") {
                let prev = i.checked_sub(1).and_then(|p| lines.get(p)).map(|p| p.trim());
                if prev != Some(OWNER_PROBE_CFG) {
                    v.push(format!("{path}:{}: launcher_probe is not behind {OWNER_PROBE_CFG}", i + 1));
                }
            }
        }
    }
    v
}

#[test]
fn probe_switches_only_in_owner_builds() {
    assert_eq!(probe_violations("src/ui.rs", "std::env::var(\"DOCK_LAUNCHER_X\")").len(), 1);
    assert!(probe_violations("src/launcher_probe.rs", "std::env::var(\"DOCK_LAUNCHER_X\")").is_empty());
    assert!(probe_violations("src/msg.rs", "pub const WM_DOCK_LAUNCHER_YIELDED: u32 = 1;").is_empty());
    assert_eq!(probe_violations("src/hook.rs", "let s = \"DOCK_LAUNCHER_MASK\";").len(), 1);
    assert_eq!(probe_violations("src/launcher/session.rs", "DOCK_TEST_FREEZE_UI_MS").len(), 1);
    assert!(probe_violations("src/hook.rs", "DOCK_TEST_FREEZE_UI_MS").is_empty());
    assert!(probe_violations("src/launcher_probe.rs", "DOCK_TEST_FREEZE_UI_MS").is_empty());
    let ok = format!("{OWNER_PROBE_CFG}\nmod launcher_probe;");
    assert!(probe_violations("src/main.rs", &ok).is_empty());
    assert_eq!(probe_violations("src/main.rs", "mod hook;\nmod launcher_probe;").len(), 1);
    assert_eq!(probe_violations("src/main.rs", "mod launcher_probe;").len(), 1);
    let use_ok = format!("fn f() {{\n    {OWNER_PROBE_CFG}\n    launcher_probe::init();\n}}");
    assert!(probe_violations("src/ui.rs", &use_ok).is_empty());
    assert_eq!(probe_violations("src/ui.rs", "fn f() {\n    launcher_probe::init();\n}").len(), 1);
    let all = scan_tree(probe_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 5: the Win32 launcher files deny panics ------------------------------------------------------------------

fn deny_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if !WIN32_FILES.contains(&path) {
        return v;
    }
    let first_item = text
        .lines()
        .position(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("//") && !t.starts_with("#![")
        })
        .unwrap_or(usize::MAX);
    match text.lines().position(|l| l.trim() == DENY) {
        Some(d) if d < first_item => {}
        _ => v.push(format!("{path}: must start with {DENY}")),
    }
    v
}

#[test]
fn launcher_win32_files_deny_panics() {
    let good = format!("//! doc\n{DENY}\n\nuse a::b;\n");
    assert!(deny_violations("src/popup_launcher.rs", &good).is_empty());
    assert_eq!(deny_violations("src/popup_launcher.rs", "use a::b;\n").len(), 1);
    assert_eq!(deny_violations("src/launcher_sys.rs", &format!("use a::b;\n{DENY}\n")).len(), 1);
    assert_eq!(deny_violations("src/launcher_files.rs", "#![deny(clippy::unwrap_used)]\nuse a;").len(), 1);
    assert!(deny_violations("src/ui.rs", "use a::b;").is_empty());
    let all = scan_tree(deny_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 6: launcher rows never send messages --------------------------------------------------------------------

fn message_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if !WIN32_FILES.contains(&path) {
        return v;
    }
    for token in ["SendMessageW", "SendMessageTimeoutW", "SendNotifyMessageW"] {
        for at in tokens(text, token) {
            v.push(format!("{path}:{}: {token} can block on a hung window", line_of(text, at)));
        }
    }
    for (at, _) in text.match_indices("GetWindowText") {
        if !text.get(..at).is_some_and(|t| t.ends_with("Internal")) {
            v.push(format!("{path}:{}: use InternalGetWindowText, it sends no message", line_of(text, at)));
        }
    }
    v
}

#[test]
fn windows_rows_never_send_messages() {
    assert_eq!(message_violations("src/launcher_sys.rs", "SendMessageW(h, 1, 0, 0);").len(), 1);
    assert_eq!(message_violations("src/popup_launcher.rs", "SendMessageTimeoutW(h);").len(), 1);
    assert_eq!(message_violations("src/launcher_files.rs", "SendNotifyMessageW(h);").len(), 1);
    assert_eq!(message_violations("src/launcher_sys.rs", "GetWindowTextW(h, &mut b);").len(), 1);
    assert!(message_violations("src/launcher_sys.rs", "InternalGetWindowText(h, &mut b);").is_empty());
    assert!(message_violations("src/launcher_sys.rs", "PostMessageW(h, 1, 0, 0);").is_empty());
    assert!(message_violations("src/ui.rs", "SendMessageW(h, 1, 0, 0);").is_empty());
    let all = scan_tree(message_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 7: launcher timers are one-shot ---------------------------------------------------------------------------

fn timer_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path != "src/popup_launcher.rs" && path != "src/launcher_sys.rs" {
        return v;
    }
    let arm = fn_body(text, "fn arm_once(", 0);
    for at in tokens(text, "SetTimer") {
        if !text.get(at + 8..).is_some_and(|t| t.starts_with('(')) {
            continue;
        }
        if !arm.is_some_and(|(s, e)| at >= s && at < e) {
            v.push(format!("{path}:{}: SetTimer outside fn arm_once", line_of(text, at)));
        }
    }
    let mut from = 0;
    while let Some((s, e)) = fn_body(text, "fn on_timer(", from) {
        let body = text.get(s..e).unwrap_or("");
        let first = body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//")).unwrap_or("");
        let kills = ["KillTimer(", "let _ = KillTimer(", "unsafe { let _ = KillTimer(", "let _ = unsafe { KillTimer("];
        if !kills.iter().any(|k| first.starts_with(k)) {
            v.push(format!("{path}:{}: fn on_timer must start with KillTimer", line_of(text, s)));
        }
        from = e;
    }
    v
}

#[test]
fn launcher_timers_are_one_shot() {
    let good = "fn arm_once(h: H) { unsafe { SetTimer(h, 1, 50, None) }; }\n\
                fn on_timer(h: H, id: usize) {\n    // one shot\n    unsafe { let _ = KillTimer(Some(h), id); }\n    work();\n}\n";
    assert!(timer_violations("src/popup_launcher.rs", good).is_empty());
    let loose = format!("{good}fn other(h: H) {{ SetTimer(h, 2, 5, None); }}\n");
    assert_eq!(timer_violations("src/launcher_sys.rs", &loose).len(), 1);
    let late = "fn on_timer(h: H, id: usize) {\n    work();\n    KillTimer(h, id);\n}\n";
    assert_eq!(timer_violations("src/popup_launcher.rs", late).len(), 1);
    assert!(timer_violations("src/ui.rs", late).is_empty());
    let cond = "fn on_timer(h: H, id: usize) {\n    if x { KillTimer(h, id); } else { work(); }\n}\n";
    assert_eq!(timer_violations("src/popup_launcher.rs", cond).len(), 1);
    // an odd quote in a comment does not shift the brace matching
    let quote = "fn on_timer(h: H, id: usize) {\n    // it's \"odd\n    KillTimer(h, id);\n}\nfn on_timer(h: H) {\n    work();\n}\n";
    assert_eq!(timer_violations("src/popup_launcher.rs", quote).len(), 1);
    assert!(timer_violations("src/popup_launcher.rs", "fn f() {}").is_empty());
    let all = scan_tree(timer_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 8: the launcher installs no mouse hook -------------------------------------------------------------------

const NO_HOOK_GUARD: &str = "kind != PopupKind::Launcher";

/// Inside `fn open(` of src/popup.rs, every `install_mouse_hook();` holds the guard on its own line or sits right under a
/// line (the nearest non-blank one) that holds the guard and ends with `{`.
fn mouse_hook_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path != "src/popup.rs" {
        return v;
    }
    let Some((s, e)) = fn_body(text, "fn open(", 0) else {
        v.push(format!("{path}: fn open( not found"));
        return v;
    };
    let first = line_of(text, s);
    let lines: Vec<&str> = text.get(s..e).unwrap_or("").lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if !l.contains("install_mouse_hook();") || l.contains(NO_HOOK_GUARD) {
            continue;
        }
        let prev = lines.get(..i).and_then(|p| p.iter().rev().find(|p| !p.trim().is_empty()));
        if !prev.is_some_and(|p| p.contains(NO_HOOK_GUARD) && p.trim_end().ends_with('{')) {
            v.push(format!("{path}:{}: install_mouse_hook() in fn open without {NO_HOOK_GUARD}", first + i));
        }
    }
    v
}

#[test]
fn no_mouse_hook_for_the_launcher() {
    let p = "src/popup.rs";
    let good = "fn open(kind: K) {\n    store();\n    if kind != PopupKind::Launcher {\n        install_mouse_hook();\n    }\n}\n";
    assert!(mouse_hook_violations(p, good).is_empty());
    let one_line = "fn open(kind: K) {\n    if kind != PopupKind::Launcher { install_mouse_hook(); }\n}\n";
    assert!(mouse_hook_violations(p, one_line).is_empty());
    let blank = "fn open(kind: K) {\n    if kind != PopupKind::Launcher {\n\n        install_mouse_hook();\n    }\n}\n";
    assert!(mouse_hook_violations(p, blank).is_empty());
    // 1.4.3's open: the call right under store_rect.
    let bare = "fn open(kind: K) {\n    store_rect(&ITEM_RECT, &anchor.item);\n    install_mouse_hook();\n}\n";
    assert_eq!(
        mouse_hook_violations(p, bare),
        ["src/popup.rs:3: install_mouse_hook() in fn open without kind != PopupKind::Launcher"]
    );
    let no_brace = "fn open(kind: K) {\n    let x = kind != PopupKind::Launcher;\n    install_mouse_hook();\n}\n";
    assert_eq!(mouse_hook_violations(p, no_brace).len(), 1);
    // Only fn open is checked; a file without it fails rather than passing empty.
    assert!(mouse_hook_violations(p, &format!("fn resume() {{\n    install_mouse_hook();\n}}\n{good}")).is_empty());
    assert_eq!(mouse_hook_violations(p, "fn close() {}").len(), 1);
    assert!(mouse_hook_violations("src/ui.rs", bare).is_empty());
    let all = scan_tree(mouse_hook_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 9: launcher timers live on the dock window --------------------------------------------------------------

/// The launcher's one-shots are armed on the dock window, never destroyed while the dock runs (the shared popup window
/// can be: Alt+F4 on another kind's popup reaches DefWindowProc, and its timers go with it), and the dock window hands
/// them to the launcher: (file, function, what its code holds).
const DOCK_TIMER_WIRING: [(&str, &str, &str); 3] = [
    ("src/launcher_sys.rs", "fn arm_once(", "timer_window()"),
    ("src/launcher_sys.rs", "fn timer_window(", "crate::hwnd()"),
    ("src/ui.rs", "fn on_timer(", "popup::on_launcher_timer(h, id)"),
];

fn dock_timer_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    for (file, sig, want) in DOCK_TIMER_WIRING {
        if path != file {
            continue;
        }
        match fn_body(text, sig, 0) {
            Some((s, e)) if code_of(text.get(s..e).unwrap_or("")).contains(want) => {}
            Some((s, _)) => v.push(format!("{path}:{}: {sig} without {want}", line_of(text, s))),
            None => v.push(format!("{path}: {sig} not found")),
        }
    }
    v
}

#[test]
fn launcher_timers_live_on_the_dock_window() {
    let p = "src/launcher_sys.rs";
    let sys = "fn timer_window() -> Option<HWND> {\n    let h = crate::hwnd();\n    (!h.0.is_null()).then_some(h)\n}\n\
               fn arm_once(id: usize, ms: u32) {\n    let Some(hwnd) = timer_window() else { return };\n}\n";
    assert!(dock_timer_violations(p, sys).is_empty());
    // The popup window: Alt+F4 on Start destroys it, and the launcher's timers with it.
    let popup = sys.replace("timer_window() else", "popup::popup_hwnd() else");
    assert_eq!(dock_timer_violations(p, &popup), ["src/launcher_sys.rs:5: fn arm_once( without timer_window()"]);
    let commented =
        sys.replace("    let Some(hwnd) = timer_window()", "    // timer_window()\n    let Some(hwnd) = popup_hwnd()");
    assert_eq!(dock_timer_violations(p, &commented).len(), 1, "a comment does not count");
    assert_eq!(dock_timer_violations(p, "fn arm_once() {}").len(), 2, "a missing fn fails");
    let ui = "fn on_timer(h: HWND, id: usize) -> Option<LRESULT> {\n    match id {\n        _ => {\n\
              if !popup::on_launcher_timer(h, id) {\n                return None;\n            }\n\
              }\n    }\n}\n";
    assert!(dock_timer_violations("src/ui.rs", ui).is_empty());
    assert_eq!(dock_timer_violations("src/ui.rs", "fn on_timer(h: HWND, id: usize) {\n    return None;\n}\n").len(), 1);
    assert!(dock_timer_violations("src/popup.rs", &popup).is_empty());
    let all = scan_tree(dock_timer_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 10: the host's own closes spare the launcher -------------------------------------------------------------

/// The closes ui.rs applies when the bar hides or moves: each goes through `close_unless_launcher` (SPEC 5.2.1).
const HOST_CLOSES: [&str; 3] = ["bar moved to the pointer display", "bar hidden (with Start)", "dock hidden"];

fn host_close_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path != "src/ui.rs" {
        return v;
    }
    let code = text.get(..test_start(text)).unwrap_or(text);
    for reason in HOST_CLOSES {
        let spared = format!("close_unless_launcher(\"{reason}\")");
        let plain = format!("close(\"{reason}\")");
        let mut found = false;
        for (i, l) in code.lines().enumerate().filter(|(_, l)| !l.trim_start().starts_with("//")) {
            found |= l.contains(&spared);
            if l.contains(&plain) {
                v.push(format!("{path}:{}: {plain} closes the launcher too", i + 1));
            }
        }
        if !found {
            v.push(format!("{path}: no {spared}"));
        }
    }
    v
}

#[test]
fn host_closes_spare_the_launcher() {
    let p = "src/ui.rs";
    let good = "fn a() {\n    popup::close_unless_launcher(\"bar moved to the pointer display\");\n}\n\
                fn b() {\n    popup::close_unless_launcher(\"bar hidden (with Start)\");\n}\n\
                fn c() {\n    crate::popup::close_unless_launcher(\"dock hidden\");\n}\n";
    assert!(host_close_violations(p, good).is_empty());
    // 1.4.3's call: the bar's hide would close the launcher.
    let reverted = good.replace("popup::close_unless_launcher(\"dock hidden\")", "popup::close(\"dock hidden\")");
    assert_eq!(
        host_close_violations(p, &reverted),
        [
            "src/ui.rs:8: close(\"dock hidden\") closes the launcher too",
            "src/ui.rs: no close_unless_launcher(\"dock hidden\")"
        ]
    );
    let extra = format!("{good}fn d() {{\n    popup::close(\"bar hidden (with Start)\");\n}}\n");
    assert_eq!(
        host_close_violations(p, &extra),
        ["src/ui.rs:11: close(\"bar hidden (with Start)\") closes the launcher too"]
    );
    let commented =
        good.replace("    popup::close_unless_launcher(\"bar moved", "    // popup::close_unless_launcher(\"bar moved");
    assert_eq!(host_close_violations(p, &commented).len(), 1, "a comment does not count");
    let in_tests =
        format!("{good}#[cfg(test)]\nmod t {{\n    fn f() {{\n        popup::close(\"dock hidden\");\n    }}\n}}\n");
    assert!(host_close_violations(p, &in_tests).is_empty(), "test code is not the host");
    assert_eq!(host_close_violations(p, "fn f() {}").len(), 3, "a ui.rs without them fails");
    assert!(host_close_violations("src/popup.rs", &reverted).is_empty());
    let all = scan_tree(host_close_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 11: the launcher opens at full alpha ---------------------------------------------------------------------

/// Inside `fn open(` of src/popup.rs: the opening alpha and the fade timer follow `initial_alpha` (the launcher's field
/// is there at once, every other kind fades in).
const OPEN_ALPHA: [&str; 2] =
    ["alpha: initial_alpha(kind),", "initial_alpha(kind) < 255 && SetTimer(Some(hwnd), TIMER_POPUP_ANIM,"];
const FADE_TIMER: &str = "SetTimer(Some(hwnd), TIMER_POPUP_ANIM,";

fn open_alpha_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path != "src/popup.rs" {
        return v;
    }
    let Some((s, e)) = fn_body(text, "fn open(", 0) else {
        v.push(format!("{path}: fn open( not found"));
        return v;
    };
    let body = code_of(text.get(s..e).unwrap_or(""));
    let line = line_of(text, s);
    for want in OPEN_ALPHA {
        if !body.contains(want) {
            v.push(format!("{path}:{line}: fn open without {want}"));
        }
    }
    // A second, unguarded fade would start the launcher transparent.
    if body.matches(FADE_TIMER).count() > body.matches(OPEN_ALPHA[1]).count() {
        v.push(format!("{path}:{line}: fn open arms TIMER_POPUP_ANIM without initial_alpha(kind) < 255"));
    }
    v
}

#[test]
fn the_launcher_opens_at_full_alpha() {
    let p = "src/popup.rs";
    let good = "fn open(kind: K) {\n    let o = Open {\n        alpha: initial_alpha(kind),\n    };\n    unsafe {\n\
                if initial_alpha(kind) < 255 && SetTimer(Some(hwnd), TIMER_POPUP_ANIM, ANIM_STEP_MS, None) == 0 {\n\
                log();\n        }\n    }\n}\n";
    assert!(open_alpha_violations(p, good).is_empty());
    // 1.4.3's open: alpha 0 and an unconditional fade.
    let old = good.replace("alpha: initial_alpha(kind),", "alpha: 0,").replace("initial_alpha(kind) < 255 && ", "");
    assert_eq!(
        open_alpha_violations(p, &old),
        [
            "src/popup.rs:1: fn open without alpha: initial_alpha(kind),",
            "src/popup.rs:1: fn open without initial_alpha(kind) < 255 && SetTimer(Some(hwnd), TIMER_POPUP_ANIM,",
            "src/popup.rs:1: fn open arms TIMER_POPUP_ANIM without initial_alpha(kind) < 255",
        ]
    );
    let twice = good.replace(
        "    unsafe {\n",
        "    unsafe {\n        SetTimer(Some(hwnd), TIMER_POPUP_ANIM, ANIM_STEP_MS, None);\n",
    );
    assert_eq!(open_alpha_violations(p, &twice).len(), 1, "a second, unguarded fade");
    let commented = good
        .replace("        alpha: initial_alpha(kind),", "        // alpha: initial_alpha(kind),\n        alpha: 0,");
    assert_eq!(open_alpha_violations(p, &commented).len(), 1, "a comment does not count");
    assert_eq!(open_alpha_violations(p, "fn close() {}"), ["src/popup.rs: fn open( not found"]);
    assert!(open_alpha_violations("src/ui.rs", &old).is_empty());
    let all = scan_tree(open_alpha_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- lint 12: the system keys are routed in one place ------------------------------------------------------------

/// The first statement of two functions of src/popup.rs: `wnd_proc` hands every message to `sys_key_msg` before its own
/// arms, and `sys_key_msg` asks `route` (the one list of the routed messages) with no filter of its own.
const ROUTED_FIRST: [(&str, &str); 2] = [
    ("fn wnd_proc(", "if let Some(r) = sys_key_msg(hwnd, msg, wp, lp) {"),
    ("fn sys_key_msg(", "match route(msg, wp.0, open_wants_sys_keys) {"),
];

fn routing_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    if path != "src/popup.rs" {
        return v;
    }
    for (sig, first) in ROUTED_FIRST {
        let Some((s, e)) = fn_body(text, sig, 0) else {
            v.push(format!("{path}: {sig} not found"));
            continue;
        };
        let body = text.get(s..e).unwrap_or("");
        let stmt = body.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//")).unwrap_or("");
        if stmt != first {
            v.push(format!("{path}:{}: {sig} must start with {first}", line_of(text, s)));
        }
    }
    v
}

#[test]
fn system_keys_are_routed_in_one_place() {
    let p = "src/popup.rs";
    let good = "fn sys_key_msg(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {\n\
                match route(msg, wp.0, open_wants_sys_keys) {\n        Route::Default => None,\n    }\n}\n\
                extern \"system\" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {\n\
                // the launcher first\n    if let Some(r) = sys_key_msg(hwnd, msg, wp, lp) {\n\
                return r;\n    }\n    match msg {\n        _ => DefWindowProcW(hwnd, msg, wp, lp),\n    }\n}\n";
    assert!(routing_violations(p, good).is_empty());
    // A second list of the messages: dropping WM_CLOSE from it would let DefWindowProc destroy the window.
    let filtered = good.replace(
        "{\nmatch route(",
        "{\n    if !matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN) {\n        return None;\n    }\nmatch route(",
    );
    assert_eq!(
        routing_violations(p, &filtered),
        ["src/popup.rs:1: fn sys_key_msg( must start with match route(msg, wp.0, open_wants_sys_keys) {"]
    );
    // An arm ahead of the launcher's routing: WM_CLOSE would reach it first.
    let ahead = good.replace(
        "    if let Some(r) = sys_key_msg",
        "    if msg == WM_CLOSE {\n        return LRESULT(0);\n    }\n    if let Some(r) = sys_key_msg",
    );
    assert_eq!(routing_violations(p, &ahead).len(), 1);
    assert_eq!(routing_violations(p, "fn open() {}").len(), 2, "a popup.rs without them fails");
    assert!(routing_violations("src/ui.rs", &ahead).is_empty());
    let all = scan_tree(routing_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- the tree walk --------------------------------------------------------------------------------------------------

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Runs a lint over every .rs file under src/ except this one; paths are given as "src/dir/file.rs".
fn scan_tree(lint: fn(&str, &str) -> Vec<String>) -> Vec<String> {
    let mut files = Vec::new();
    collect(Path::new(SRC), &mut files);
    let mut out = Vec::new();
    for f in files {
        let rel = f.strip_prefix(SRC).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let path = format!("src/{}", rel.trim_start_matches('/'));
        if path == "src/launcher/lints.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        out.extend(lint(&path, &text));
    }
    out
}

#[test]
fn tree_walk_finds_the_sources() {
    let mut files = Vec::new();
    collect(Path::new(SRC), &mut files);
    assert!(files.iter().any(|f| f.ends_with("main.rs")));
    assert!(files.iter().any(|f| f.to_string_lossy().replace('\\', "/").ends_with("launcher/lints.rs")));
}
