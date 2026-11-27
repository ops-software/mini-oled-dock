//! Lints of the files package (plan rules D1, D5, D8, D9): plain text scans of every source under apps/files, as the
//! dock's own lints do. A comment line (starting with //) never counts; every other line does, string literals
//! included, so a lint cannot be satisfied by a comment. Each lint has seeded cases through the same scanner function.
//! Tokens that the workspace's own spawn lint would read as a spawn are built with concat!.

use std::path::{Path, PathBuf};

/// This file: never scanned by its own lints (it names every token).
const SELF: &str = "tests/lints.rs";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// (path relative to apps/files with '/', text) of every .rs file under src, tests and examples, and of build.rs.
fn rust_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for d in ["src", "tests", "examples"] {
        collect(&root().join(d), &mut files);
    }
    files.push(root().join("build.rs"));
    let mut out = Vec::new();
    for f in files.into_iter().filter(|f| f.extension().is_some_and(|x| x == "rs")) {
        let rel = f.strip_prefix(root()).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        if rel == SELF {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        out.push((rel, text));
    }
    assert!(out.iter().any(|(p, _)| p == "src/own_files.rs"), "the walk finds the sources");
    assert!(out.iter().any(|(p, _)| p == "build.rs"), "the walk reads build.rs");
    out
}

fn scan(lint: fn(&str, &str) -> Vec<String>) -> Vec<String> {
    rust_sources().iter().flat_map(|(p, t)| lint(p, t)).collect()
}

// ---- D1: user files change only through IFileOperation in the ops host --------------------------------------------

/// The files that may change files themselves: the app's own data folder, the bin's $I records of restored items
/// (removed through the handle that proved them, never by name), the two test fences and the fault DLL.
const MUTATION_ALLOWED: [&str; 5] = [
    "src/own_files.rs",
    "src/ops/binrec.rs",
    "tests/common/temp.rs",
    "examples/common/live.rs",
    "examples/files_faultext/lib.rs",
];

/// The files that may also open a file handle with CreateFileW: the change watcher, icons.bin's reader, the pipe and
/// the hand-off's folder pin.
const CREATE_FILE_ALLOWED: [&str; 5] =
    ["src/watch/iocp.rs", "src/decor/iconsbin.rs", "src/host/named.rs", "src/ops_client.rs", "src/instance.rs"];

/// Calls that create, change, move or delete a file. The Win32 names are stems, so every variant matches too (A and
/// W, Ex, WithProgress, 2, FromApp, Transacted); GetFileAttributesW does not match the SetFileAttributes stem.
const MUTATIONS: [&str; 27] = [
    "DeleteFile",
    "RemoveDirectory",
    "CreateDirectory",
    "MoveFile",
    "CopyFile",
    "ReplaceFile",
    "CreateHardLink",
    "CreateSymbolicLink",
    "SetFileAttributes",
    "SetFileTime",
    "FSCTL_SET_REPARSE_POINT",
    "SHFileOperation",
    "SetFileInformationByHandle",
    "fs::remove_file",
    "fs::remove_dir",
    "fs::rename",
    "fs::copy",
    "fs::write",
    "fs::create_dir",
    "fs::set_permissions",
    "fs::hard_link",
    "fs::soft_link",
    "symlink_file",
    "symlink_dir",
    "File::create",
    "File::options",
    "OpenOptions",
];

/// The same std::fs functions imported by name (`use std::fs::{rename, write}`).
const FS_NAMES: [&str; 9] =
    ["remove_file", "remove_dir", "rename", "copy", "write", "create_dir", "set_permissions", "hard_link", "soft_link"];

const CREATE_FILE: [&str; 3] = ["CreateFileW", "CreateFileA", "CreateFile2"];

fn has_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + word.len()..].chars().next();
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

fn mutation_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mutation_ok = MUTATION_ALLOWED.contains(&path);
    let create_ok = mutation_ok || CREATE_FILE_ALLOWED.contains(&path);
    for (n, line) in text.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        if !mutation_ok {
            for t in MUTATIONS {
                if line.contains(t) {
                    v.push(format!("{path}:{}: {t} outside the ops host's IFileOperation", n + 1));
                }
            }
            let imports_fs = line.contains("std::fs::{") || line.contains("fs::{") || line.contains("std::fs as");
            for t in FS_NAMES {
                if imports_fs && has_word(line, t) {
                    v.push(format!("{path}:{}: std::fs::{t} imported outside the ops host", n + 1));
                }
            }
            if line.contains("std::fs as") {
                v.push(format!("{path}:{}: std::fs renamed (hides its calls from this lint)", n + 1));
            }
        }
        if !create_ok {
            for t in CREATE_FILE {
                if line.contains(t) {
                    v.push(format!("{path}:{}: {t} outside the files allowed to open handles", n + 1));
                }
            }
        }
    }
    v
}

#[test]
fn mutations_go_through_ifileoperation() {
    let bad = "src/listing/store.rs";
    // One bad snippet per listed name, the calls behind TempRoot's and LiveRoot's methods included.
    let snippets = [
        "unsafe { DeleteFileW(w) }?;",
        "unsafe { DeleteFileA(s) }?;",
        "unsafe { DeleteFileFromAppW(w) }?;",
        "unsafe { RemoveDirectoryW(w) }?;",
        "unsafe { CreateDirectoryW(w, None) }?;",
        "unsafe { CreateDirectoryExW(t, w, None) }?;",
        "MoveFileW(a, b)",
        "unsafe { MoveFileExW(a, b, MOVEFILE_REPLACE_EXISTING) }?;",
        "MoveFileWithProgressW(a, b, None, None, 0)",
        "MoveFileTransactedW(a, b, None, None, 0, tx)",
        "CopyFileW(a, b, false)",
        "CopyFileExW(a, b, None, None, None, 0)",
        "CopyFile2(a, b, None)",
        "ReplaceFileW(a, b, None, 0, None, None)",
        "CreateHardLinkW(a, b, None)",
        "CreateSymbolicLinkW(a, b, 0)",
        "unsafe { SetFileAttributesW(w, FILE_ATTRIBUTE_NORMAL) }?;",
        "unsafe { SetFileTime(h, None, None, Some(&ft)) }?;",
        "DeviceIoControl(h, FSCTL_SET_REPARSE_POINT, Some(b), n, None, 0, None, None)",
        "SHFileOperationW(&mut op)",
        "SetFileInformationByHandle(h, FileRenameInfo, p, n)",
        "std::fs::remove_file(&p)?;",
        "fs::remove_dir(&p)?;",
        "fs::remove_dir_all(&p)?;",
        "std::fs::rename(&a, &b).unwrap();",
        "fs::copy(&a, &b)?;",
        "std::fs::write(&p, bytes)?;",
        "fs::create_dir(&p)?;",
        "fs::create_dir_all(&p)?;",
        "fs::set_permissions(&p, perm)?;",
        "fs::hard_link(&a, &b)?;",
        "fs::soft_link(&a, &b)?;",
        "std::os::windows::fs::symlink_file(&a, &b)?;",
        "std::os::windows::fs::symlink_dir(&a, &b)?;",
        "let f = File::create(&p)?;",
        "let f = File::options().write(true).open(&p)?;",
        "let f = OpenOptions::new().write(true).open(\"a.txt:s\")?;",
        "use std::fs::{read, write};",
        "use std::fs::{remove_file as rm};",
        "use std::fs as f;",
        "std::fs::write(&p, b); // allowed in tests",
    ];
    for s in snippets {
        assert!(!mutation_violations(bad, s).is_empty(), "not caught: {s}");
        for ok in MUTATION_ALLOWED {
            assert!(mutation_violations(ok, s).is_empty(), "{ok} may do it: {s}");
        }
    }
    assert!(mutation_violations(bad, "// std::fs::write(&p, b)").is_empty(), "a comment line does not count");
    assert!(mutation_violations(bad, "/// OpenOptions in a doc line").is_empty());
    assert!(
        mutation_violations(bad, "let b = std::fs::read(&p)?; let f = File::open(&p)?;").is_empty(),
        "reads are fine"
    );
    assert!(mutation_violations(bad, "use std::fs::{self, File};").is_empty());
    let read_attrs = "let a = unsafe { GetFileAttributesW(w) };";
    assert!(mutation_violations(bad, read_attrs).is_empty(), "reading attributes is fine");
    assert!(
        mutation_violations(bad, "root.rename(\"a\", \"b\"); root.write(\"a\", b\"x\"); root.remove(\"a\");")
            .is_empty(),
        "TempRoot's methods are the allowed route"
    );
    let create = "let h = unsafe { CreateFileW(w, GENERIC_READ.0, share, None, OPEN_EXISTING, flags, None) }?;";
    assert!(!mutation_violations(bad, create).is_empty());
    for ok in CREATE_FILE_ALLOWED.iter().chain(MUTATION_ALLOWED.iter()) {
        assert!(mutation_violations(ok, create).is_empty(), "{ok}");
    }
    assert!(mutation_violations(bad, "CreateFileMappingW(h, None, PAGE_READWRITE, 0, n, None)").is_empty());
    assert!(!mutation_violations("src/watch/iocp.rs", "std::fs::write(&p, b)?;").is_empty(), "only CreateFileW there");
    let all = scan(mutation_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- D5: no code in apps/files writes the registry ------------------------------------------------------------------

const REGISTRY_WRITES: [&str; 20] = [
    "RegSetValue",
    "RegSetKeyValue",
    "RegCreateKey",
    "RegDeleteKey",
    "RegDeleteValue",
    "RegDeleteTree",
    "RegCopyTree",
    "RegRenameKey",
    "RegRestoreKey",
    "RegLoadKey",
    "RegReplaceKey",
    "RegSetKeySecurity",
    "SHSetValue",
    "SHDeleteKey",
    "SHDeleteValue",
    "SHDeleteEmptyKey",
    "SHRegSetUSValue",
    "SHRegWriteUSValue",
    "SHCopyKey",
    "reg.exe",
];

fn registry_violations(path: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        for t in REGISTRY_WRITES {
            if line.contains(t) {
                v.push(format!("{path}:{}: {t} writes the registry", n + 1));
            }
        }
    }
    v
}

#[test]
fn registry_is_read_only() {
    for s in [
        "RegSetValueExW(key, name, 0, REG_DWORD, Some(&b))",
        "RegSetKeyValueW(key, sub, name, REG_SZ, p, n)",
        "RegCreateKeyExW(HKEY_CURRENT_USER, sub, 0, None, 0, KEY_WRITE, None, &mut k, None)",
        "RegDeleteKeyW(key, sub)",
        "RegDeleteValueW(key, name)",
        "RegDeleteTreeW(key, sub)",
        "SHSetValueW(key, sub, name, REG_DWORD, p, 4)",
        "SHDeleteKeyW(key, sub)",
        "run(\"reg.exe\", &args)",
    ] {
        assert_eq!(registry_violations("src/ops/classify.rs", s).len(), 1, "not caught: {s}");
    }
    let read = "RegOpenKeyExW(HKEY_CURRENT_USER, sub, 0, KEY_READ, &mut k); RegGetValueW(k, None, name, RRF_RT_REG_DWORD, None, p, n)";
    assert!(registry_violations("src/ops/classify.rs", read).is_empty(), "reads are fine");
    assert!(registry_violations("src/ops/classify.rs", "// RegSetValueExW is never called").is_empty());
    let all = scan(registry_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- D8: every process a test, probe or role starts carries CREATE_NO_WINDOW --------------------------------------

/// Lines around a spawn that may hold the flag: the next 6 after a std spawn, 6 before or after a Win32 one.
const SPAWN_WINDOW: usize = 6;

/// A Win32 process start on the line: the CreateProcess stem with any suffix (A, W, AsUserW, WithTokenW,
/// WithLogonW) followed by its opening parenthesis.
fn starts_win_spawn(line: &str) -> bool {
    let stem = "CreateProcess";
    line.match_indices(stem).any(|(i, _)| {
        line[i + stem.len()..].trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_').starts_with('(')
    })
}

fn spawn_violations(path: &str, text: &str) -> Vec<String> {
    let std_spawn = concat!("Command::", "new(");
    let flags = concat!("creation_", "flags(");
    let lines: Vec<&str> = text.lines().collect();
    let mut v = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if is_comment(line) || line.trim_start().starts_with("use ") {
            continue;
        }
        let starts_std = line.contains(std_spawn);
        let starts_win = starts_win_spawn(line);
        if !starts_std && !starts_win {
            continue;
        }
        let from = if starts_win { n.saturating_sub(SPAWN_WINDOW) } else { n };
        let to = (n + SPAWN_WINDOW + 1).min(lines.len());
        let near: Vec<&str> = lines[from..to].iter().copied().filter(|l| !is_comment(l)).collect();
        let near = near.join("\n");
        if !(near.contains("CREATE_NO_WINDOW") && (starts_win || near.contains(flags))) {
            v.push(format!("{path}:{}: a process is started without CREATE_NO_WINDOW", n + 1));
        }
    }
    v
}

#[test]
fn no_window_spawns() {
    let (new, create) = (concat!("Command::", "new("), concat!("CreateProcess", "W("));
    let good = format!("let mut c = {new}exe);\nc.creation_flags(CREATE_NO_WINDOW)\n    .spawn();");
    assert!(spawn_violations("tests/a.rs", &good).is_empty());
    let bad = format!("let c = {new}exe)\n    .spawn();");
    assert_eq!(spawn_violations("tests/a.rs", &bad).len(), 1);
    let flags_only = format!("let c = {new}exe).creation_flags(0).spawn();");
    assert_eq!(spawn_violations("src/a.rs", &flags_only).len(), 1, "creation_flags without the flag");
    let far = format!("{new}x)\n{}\n.creation_flags(CREATE_NO_WINDOW)", "\n".repeat(8));
    assert_eq!(spawn_violations("src/a.rs", &far).len(), 1, "the flag must stand next to the spawn");
    let commented = format!("{new}x)\n// .creation_flags(CREATE_NO_WINDOW)\n.spawn()");
    assert_eq!(spawn_violations("src/a.rs", &commented).len(), 1, "a comment does not count");
    let win_good = format!("unsafe {{ {create}app, line, None, None, true,\n    CREATE_NO_WINDOW | CREATE_SUSPENDED, None, dir, &si, &mut pi) }}");
    assert!(spawn_violations("src/host/proc.rs", &win_good).is_empty());
    let win_before = format!("let flags = CREATE_NO_WINDOW | CREATE_SUSPENDED;\nunsafe {{ {create}app, line) }};");
    assert!(spawn_violations("src/host/proc.rs", &win_before).is_empty());
    assert_eq!(spawn_violations("src/host/proc.rs", &format!("unsafe {{ {create}app, line) }};")).len(), 1);
    for suffix in ["A", "AsUserW", "WithTokenW", "WithLogonW"] {
        let call = format!("unsafe {{ CreateProcess{suffix}(app, line) }};");
        assert_eq!(spawn_violations("src/host/proc.rs", &call).len(), 1, "not caught: {call}");
    }
    assert!(spawn_violations("src/a.rs", &format!("/// {create}x)\n// {new}x)\nuse std::process::Command;")).is_empty());
    let all = scan(spawn_violations);
    assert!(all.is_empty(), "{all:#?}");
}

// ---- D9: the app's display name is not in code ----------------------------------------------------------------------

/// The display name, spelled in pieces so this file does not hold it.
const DISPLAY_NAME: &str = concat!("fo", "lio");

fn display_name_violations(path: &str, bytes: &[u8]) -> Vec<String> {
    let lower = bytes.to_ascii_lowercase();
    let needle = DISPLAY_NAME.as_bytes();
    let hits = lower.windows(needle.len()).filter(|w| *w == needle).count();
    if hits == 0 {
        Vec::new()
    } else {
        vec![format!("{path}: the display name appears {hits} time(s)")]
    }
}

#[test]
fn display_name_is_not_in_code() {
    let name = DISPLAY_NAME;
    let upper = name.to_uppercase();
    let title = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
    for s in [format!("const NAME: &str = \"{title}\";"), format!("// {upper} window"), format!("mod_{name}_x")] {
        assert_eq!(display_name_violations("src/a.rs", s.as_bytes()).len(), 1, "not caught: {s}");
    }
    assert!(display_name_violations("src/a.rs", b"files, mod-files, FILES_DATA_DIR").is_empty());
    // No file is allowlisted: the name lives in the kit's table (kit.app.files), so a second key or a comment fails.
    let key_line = format!("\"files.app.name\" = \"{title}\"\n\"files.app.other\" = \"{title} Beta\"");
    assert_eq!(display_name_violations("strings/en.toml", key_line.as_bytes()).len(), 1, "a key holding the name");
    let comment = format!("// the {title} window\npub fn x() {{}}");
    assert_eq!(display_name_violations("src/window.rs", comment.as_bytes()).len(), 1, "a comment holding the name");
    let mut files = Vec::new();
    collect(&root(), &mut files);
    assert!(files.iter().any(|f| f.ends_with("Cargo.toml")), "the walk reads every file, the manifest included");
    let mut all = Vec::new();
    for f in files {
        let rel = f.strip_prefix(root()).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        // The golden base set is the runner's rendering of the scenes, not code: the About scene shows the name the
        // kit's table gives it, and a new name regenerates the set.
        if rel.starts_with("tests/golden/base/") {
            continue;
        }
        let bytes = std::fs::read(&f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        all.extend(display_name_violations(&rel, &bytes));
    }
    assert!(all.is_empty(), "{all:#?}");
}

// ---- ROADMAP 3.1 gate a: the window modules show no literal text --------------------------------------------------

/// The line of src/lib.rs under which the window's modules are declared (P3-files-app FZ1).
const WINDOW_MARKER: &str = "// ---- P3-files-app (window)";

/// Calls whose string-literal arguments would reach the screen.
const UI_CALLS: [&str; 8] = [
    "TextLabel::new(",
    "Button::new(",
    "IconButton::new(",
    "Text::from(",
    "Text::raw(",
    "set_text(",
    "set_label(",
    "tf(",
];

/// Struct literals and the field of each that reaches the screen.
const UI_FIELDS: [(&str, &str); 5] = [
    ("Command {", "label:"),
    ("Notice {", "text:"),
    ("MenuItem::Custom {", "label:"),
    ("ToastSpec {", "text:"),
    ("WindowSpec {", "title:"),
];

/// A line holding one of these may hold literals: logs, wide-string macros, assertions, registered and class names.
const NOT_UI: [&str; 7] = ["log!(", "w!(", "debug_assert", "panic!(", "mini-oled-dock.", "MiniOled", "// lint: not UI"];

/// Lines a struct literal may span before the scan gives up on it.
const FIELD_SPAN: usize = 40;

/// The string literals of `s` with their byte offsets: (start of the opening quote, text between the quotes).
fn literals(s: &str) -> Vec<(usize, &str)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' => {
                // A char literal ('"', '\'') is skipped whole; a lifetime is a lone apostrophe.
                if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
                    i += 4;
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                out.push((start, s.get(start + 1..i.min(b.len())).unwrap_or("")));
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// A literal at `at` that names a placeholder: the first element of a tuple in a slice, as in tf's &[("name", value)].
fn is_placeholder_name(line: &str, at: usize, text: &str) -> bool {
    let before = line[..at].trim_end();
    let after = line.get(at + text.len() + 2..).unwrap_or("").trim_start();
    (before.ends_with("[(") || before.ends_with(", (") || before.ends_with(",(")) && after.starts_with(',')
}

/// True when a literal from byte `from` on holds a letter and is not a placeholder name.
fn has_text_literal(line: &str, from: usize) -> bool {
    literals(line)
        .into_iter()
        .any(|(at, text)| at >= from && text.chars().any(char::is_alphabetic) && !is_placeholder_name(line, at, text))
}

/// The lines of `text` that are not inside a #[cfg(test)] item.
fn outside_tests(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut pending, mut depth, mut inside) = (false, 0i32, false);
    for (n, line) in text.lines().enumerate() {
        if inside {
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth <= 0 {
                inside = false;
            }
            continue;
        }
        if line.trim_start().starts_with("#[cfg(test)]") {
            pending = true;
            continue;
        }
        if pending {
            if line.contains('{') {
                depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
                inside = depth > 0;
                pending = false;
            } else if line.trim_end().ends_with(';') {
                pending = false;
            }
            continue;
        }
        out.push((n, line));
    }
    out
}

fn literal_ui_violations(path: &str, text: &str) -> Vec<String> {
    let lines = outside_tests(text);
    let allowed = |l: &str| is_comment(l) || NOT_UI.iter().any(|t| l.contains(t));
    let mut v = Vec::new();
    for (i, &(n, line)) in lines.iter().enumerate() {
        if allowed(line) {
            continue;
        }
        for call in UI_CALLS {
            if line.match_indices(call).any(|(at, _)| has_text_literal(line, at + call.len())) {
                v.push(format!("{path}:{}: a literal text passed to {call}..)", n + 1));
            }
        }
        for (head, field) in UI_FIELDS {
            let Some(start) = line.find(head) else { continue };
            // The literal's lines: from the head to the brace that closes it.
            let mut depth = 0i32;
            for (k, &(m, l)) in lines.iter().enumerate().skip(i).take(FIELD_SPAN) {
                let part = if k == i { &l[start..] } else { l };
                if !allowed(l) {
                    if let Some(at) = part.find(field) {
                        let from = l.len() - part.len() + at + field.len();
                        if has_text_literal(l, from) {
                            v.push(format!("{path}:{}: a literal text in {head} {field} ..}}", m + 1));
                        }
                    }
                }
                depth += part.matches('{').count() as i32 - part.matches('}').count() as i32;
                if depth <= 0 {
                    break;
                }
            }
        }
    }
    v
}

/// The window's module files: every module declared under the marker of src/lib.rs (a file, or a folder and every
/// .rs file in it), fakes.rs excepted.
fn window_sources() -> Vec<(String, String)> {
    let lib = std::fs::read_to_string(root().join("src/lib.rs")).unwrap_or_else(|e| panic!("read src/lib.rs: {e}"));
    let (_, below) = lib.split_once(WINDOW_MARKER).unwrap_or_else(|| panic!("src/lib.rs has no window marker"));
    let mut out = Vec::new();
    for name in
        below.lines().filter_map(|l| l.trim().strip_prefix("pub mod")).filter_map(|r| r.trim().strip_suffix(';'))
    {
        let name = name.trim();
        if name == "fakes" {
            continue;
        }
        let mut files = Vec::new();
        let single = root().join("src").join(format!("{name}.rs"));
        if single.is_file() {
            files.push(single);
        }
        collect(&root().join("src").join(name), &mut files);
        for f in files.into_iter().filter(|f| f.extension().is_some_and(|x| x == "rs")) {
            let rel = f.strip_prefix(root()).unwrap_or(&f).to_string_lossy().replace('\\', "/");
            let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
            out.push((rel, text));
        }
    }
    assert!(out.iter().any(|(p, _)| p == "src/tr.rs"), "the window's modules are found under the marker");
    out
}

#[test]
fn no_literal_ui_strings() {
    let bad = "src/window.rs";
    for s in [
        "let l = TextLabel::new(id, Text::from(\"Back\"), role);",
        "s.notice(Notice { text: Text::from(\"Done\"), action: None, hold_ms: 6_000 });",
        "let n = Notice {\n    text: \"Done\".into(),\n    action: None,\n};",
        "let c = Command { id, label: \"Open\".into(), glyph: None };",
        "items.push(MenuItem::Custom { id, label: Text::Borrowed(\"Open\"), glyph: None });",
        "let spec = WindowSpec {\n    class: WINDOW_CLASS,\n    title: \"Files\".into(),\n};",
        "out.set_text(CellKind::Text, \"folder\");",
        "let t = tr::tf(k::FILES_NAV_BACK_TO, lang, &[(\"name\", \"Docs\")]);",
        "cx.toast(ToastSpec { text: \"Copied\".into(), action: None, progress: None, hold_ms: None });",
    ] {
        assert!(!literal_ui_violations(bad, s).is_empty(), "not caught: {s}");
    }
    for s in [
        "let l = TextLabel::new(id, Text::from(\"Back\"), role); // lint: not UI",
        "s.notice(Notice { text: Text::from(\"Done\"), action: None, hold_ms: 6_000 }); // lint: not UI",
        "let t = tr::tf(k::FILES_NAV_BACK_TO, lang, &[(\"name\", &name)]);",
        "let t = tf(k::FILES_STATUS_ITEMS_OTHER, lang, &[(\"n\", &n), (\"size\", &size)]);",
        "oled_sys::log!(\"files: notice: {}\", n.text);",
        "let spec = WindowSpec { class: \"MiniOledFiles\", title: tr::t(k::FILES_APP_BETA, lang) };",
        "let n = Notice { text: tr::t(k::FILES_OPS_HOST_LOST, lang), action: None, hold_ms: 6_000 };",
        "let x = Text::from(\"\u{2192} 12 %\".to_string()); // digits and signs only are not words",
        "// TextLabel::new(id, Text::from(\"Back\"), role)",
        "#[cfg(test)]\nmod tests {\n    fn f() { let _ = Text::from(\"Back\"); }\n}",
        "out.set_text(CellKind::Text, &name);",
        "let c = if c == '\"' { Text::from(sep) } else { t(k, lang) };",
    ] {
        assert!(literal_ui_violations(bad, s).is_empty(), "wrongly caught: {s}: {:?}", literal_ui_violations(bad, s));
    }
    let all: Vec<String> = window_sources().iter().flat_map(|(p, t)| literal_ui_violations(p, t)).collect();
    assert!(all.is_empty(), "{all:#?}");
}

// ---- U1, U6: only the start path resolves the window's real folders ---------------------------------------------

/// What resolves the own data folder or files.json's folder.
const REAL_FOLDERS: [&str; 6] = [
    "OwnDir::default_dir",
    "options::spec(",
    "FOLDERID_RoamingAppData",
    "FOLDERID_LocalAppData",
    "\"APPDATA\"",
    "\"LOCALAPPDATA\"",
];

/// The one call that builds the real services: on the start path only.
const REAL_SERVICES: &str = "services::real(";

/// The 0-based line range of the body of the first function whose signature holds `sig`.
fn fn_body(text: &str, sig: &str) -> Option<std::ops::Range<usize>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| !is_comment(l) && l.contains(sig))?;
    let mut depth = 0i32;
    let mut opened = false;
    for (n, l) in lines.iter().enumerate().skip(start) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        opened |= l.contains('{');
        if opened && depth <= 0 {
            return Some(start..n + 1);
        }
    }
    Some(start..lines.len())
}

fn real_folder_violations(path: &str, text: &str) -> Vec<String> {
    // Where each may appear: services.rs's real(), options.rs's spec(), selftest.rs (and real() only in app.rs).
    let allowed = match path {
        "src/services.rs" => fn_body(text, "pub fn real("),
        "src/options.rs" => fn_body(text, "pub fn spec("),
        "src/selftest.rs" => Some(0..usize::MAX),
        _ => None,
    };
    let mut v = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let inside = allowed.as_ref().is_some_and(|r| r.contains(&n));
        for t in REAL_FOLDERS {
            if line.contains(t) && !inside {
                v.push(format!("{path}:{}: {t} outside the start path", n + 1));
            }
        }
        if line.contains(REAL_SERVICES) && path != "src/app.rs" {
            v.push(format!("{path}:{}: {REAL_SERVICES}..) outside app.rs", n + 1));
        }
    }
    v
}

#[test]
fn real_folders_only_on_the_start_path() {
    let services = "pub fn real(hub: &Arc<Hub>) -> Services {\n    let data = OwnDir::default_dir();\n}\nfn other() {\n    let d = OwnDir::default_dir();\n}";
    assert_eq!(
        real_folder_violations("src/services.rs", services).len(),
        1,
        "only real()'s body may resolve the folder"
    );
    let options = "pub fn spec() -> Option<FileSpec> {\n    known(&FOLDERID_RoamingAppData)\n}\npub fn x() { std::env::var(\"APPDATA\") }";
    assert_eq!(real_folder_violations("src/options.rs", options).len(), 1, "only spec()'s body");
    for bad in [
        "let d = OwnDir::default_dir();",
        "let s = options::spec();",
        "SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)",
        "let p = std::env::var(\"LOCALAPPDATA\");",
        "let s = services::real(&hub, exe, None);",
    ] {
        for path in ["src/window.rs", "src/fakes.rs", "src/preview/f1a.rs", "tests/ui_f1a.rs"] {
            assert!(!real_folder_violations(path, bad).is_empty(), "{path}: not caught: {bad}");
        }
    }
    assert!(real_folder_violations("src/app.rs", "let s = services::real(&hub, exe, None);").is_empty());
    assert!(real_folder_violations("src/selftest.rs", "let s = options::spec();").is_empty());
    assert!(real_folder_violations("tests/ui_open.rs", "let s = services::real_shell(&hub, exe, None);").is_empty());
    assert!(real_folder_violations("src/window.rs", "// OwnDir::default_dir() is app.rs's").is_empty());
    // Every window module (fakes.rs and preview/* included) and every test but this lint.
    let mut files = window_sources();
    let fakes = root().join("src/fakes.rs");
    files.push((
        "src/fakes.rs".into(),
        std::fs::read_to_string(&fakes).unwrap_or_else(|e| panic!("read fakes.rs: {e}")),
    ));
    files.extend(rust_sources().into_iter().filter(|(p, _)| p.starts_with("tests/")));
    let all: Vec<String> = files.iter().flat_map(|(p, t)| real_folder_violations(p, t)).collect();
    assert!(all.is_empty(), "{all:#?}");
}
