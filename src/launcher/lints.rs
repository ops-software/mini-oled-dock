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
    for (i, c) in text.get(open..)?.char_indices() {
        if in_str {
            if c == '"' && prev != '\\' {
                in_str = false;
            }
            prev = if prev == '\\' && c == '\\' { ' ' } else { c };
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
        if let (Some(b), Some(e)) = (text.find(begin), text.find(end)) {
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
        let stmt =
            body.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with("//")).collect::<Vec<_>>().join("\n");
        let first = stmt.split(';').next().unwrap_or("");
        if !first.contains("KillTimer(") {
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
    assert!(timer_violations("src/popup_launcher.rs", "fn f() {}").is_empty());
    let all = scan_tree(timer_violations);
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
