//! launcher-probe: read-only probe for the launcher's L0 owner session (plan L1, task L0-1).
//!
//! Every subcommand prints JSON lines to stdout and nothing else: the tool writes no file, no registry value
//! and no window message, and opens no handle for writing (process handles are PROCESS_QUERY_LIMITED_INFORMATION,
//! tokens are TOKEN_QUERY, except the shell user's known-folder lookup which also asks for the duplicate and
//! impersonate rights SHGetKnownFolderPath needs, as the dock's own start_import.rs does).
//! It installs no keyboard hook. The owner redirects stdout to a file.

use std::fmt::Write as _;

// ---------------------------------------------------------------------------------------------------------------
// Pure parts (unit tested).
// ---------------------------------------------------------------------------------------------------------------

/// Why a scope root or a search word was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    ControlChar,
    DoubleQuote,
    TooManyWords,
    WordTooLong,
}

impl Refusal {
    fn text(&self) -> &'static str {
        match self {
            Refusal::Empty => "empty input",
            Refusal::ControlChar => "control character",
            Refusal::DoubleQuote => "double quote in a search word",
            Refusal::TooManyWords => "more than 8 words",
            Refusal::WordTooLong => "word longer than 64 characters",
        }
    }
}

/// Doubles every single quote (SQL string literal escaping).
fn quote_sql(s: &str) -> String {
    s.replace('\'', "''")
}

/// "SCOPE='file:<root>'" clauses joined by OR; every ' is doubled and a control character is refused.
pub fn scope_clause(roots: &[&str]) -> Result<String, &'static str> {
    if roots.is_empty() {
        return Err("no roots");
    }
    let mut parts = Vec::with_capacity(roots.len());
    for r in roots {
        if r.is_empty() {
            return Err("empty root");
        }
        if r.chars().any(|c| c.is_control()) {
            return Err("control character in a root");
        }
        parts.push(format!("SCOPE='file:{}'", quote_sql(r)));
    }
    Ok(parts.join(" OR "))
}

/// Route b: hand-written SQL on Search.CollatorDSO. At most 8 words of at most 64 characters, each a word-prefix
/// CONTAINS term; ' is doubled, " and control characters are refused. "scope" is a ready clause from scope_clause.
pub fn fallback_sql(words: &[&str], scope: &str) -> Result<String, Refusal> {
    if words.is_empty() {
        return Err(Refusal::Empty);
    }
    if words.len() > 8 {
        return Err(Refusal::TooManyWords);
    }
    let mut terms = Vec::with_capacity(words.len());
    for w in words {
        if w.is_empty() {
            return Err(Refusal::Empty);
        }
        if w.chars().count() > 64 {
            return Err(Refusal::WordTooLong);
        }
        if w.chars().any(|c| c.is_control()) {
            return Err(Refusal::ControlChar);
        }
        if w.contains('"') {
            return Err(Refusal::DoubleQuote);
        }
        terms.push(format!("CONTAINS(System.FileName, '\"{}*\"')", quote_sql(w)));
    }
    Ok(format!("SELECT {} FROM SystemIndex WHERE ({}) AND {}", SELECT_COLUMNS, scope, terms.join(" AND ")))
}

const SELECT_COLUMNS: &str = "System.ItemPathDisplay, System.FileName, System.ItemFolderNameDisplay, System.ItemType";

/// JSON string literal (quotes included) for any text.
fn jstr(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// File name (after the last \ or /) of an image path.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// S-PT: PowerToys*.exe and Microsoft.CmdPal*.exe, case-insensitive.
pub fn is_pt_image(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    f.ends_with(".exe") && (f.starts_with("powertoys") || f.starts_with("microsoft.cmdpal"))
}

/// Named link of shell:AppsFolder (SPEC 5.1): a .url parsing name, or a folder.
pub fn is_named_link(parsing_name: &str, is_folder: bool) -> bool {
    is_folder || parsing_name.to_ascii_lowercase().ends_with(".url")
}

/// cmdpal-items: leaf values (string, number, bool) whose key path mentions bookmarks, extensions, usage, counts or
/// enabled flags, as {"path","value"} pairs. No guess about the file layout beyond those words. At most "cap" pairs.
pub fn scan_cmdpal(v: &serde_json::Value, cap: usize) -> Vec<(String, String)> {
    fn walk(v: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<(String, String)>, cap: usize) {
        if out.len() >= cap {
            return;
        }
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    path.push(k.clone());
                    walk(x, path, out, cap);
                    path.pop();
                }
            }
            serde_json::Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    path.push(i.to_string());
                    walk(x, path, out, cap);
                    path.pop();
                }
            }
            leaf => {
                let p = path.join(".");
                let l = p.to_ascii_lowercase();
                let keep = ["bookmark", "extension", "usage", "count", "enabled"].iter().any(|w| l.contains(w));
                let text = match leaf {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    serde_json::Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                };
                if let (true, Some(t)) = (keep, text) {
                    out.push((p, t));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(v, &mut Vec::new(), &mut out, cap);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_clause_quoting() {
        assert_eq!(scope_clause(&["C:\\Users\\a"]).unwrap(), "SCOPE='file:C:\\Users\\a'");
        assert_eq!(scope_clause(&["C:\\O'Neil"]).unwrap(), "SCOPE='file:C:\\O''Neil'");
        assert_eq!(scope_clause(&["C:\\''"]).unwrap(), "SCOPE='file:C:\\'''''");
        assert_eq!(scope_clause(&["C:\\a", "C:\\b"]).unwrap(), "SCOPE='file:C:\\a' OR SCOPE='file:C:\\b'");
        // a root that tries to close the literal stays inside it
        let c = scope_clause(&["x' OR 1=1 --"]).unwrap();
        assert_eq!(c, "SCOPE='file:x'' OR 1=1 --'");
        // % and -- are data in a SCOPE literal
        assert_eq!(scope_clause(&["C:\\100%--"]).unwrap(), "SCOPE='file:C:\\100%--'");
    }

    #[test]
    fn scope_clause_refuses() {
        assert!(scope_clause(&[]).is_err());
        assert!(scope_clause(&[""]).is_err());
        assert!(scope_clause(&["C:\\a\nb"]).is_err());
        assert!(scope_clause(&["C:\\a\u{0}"]).is_err());
        assert!(scope_clause(&["C:\\ok", "C:\\a\u{7f}"]).is_err());
        assert!(scope_clause(&["C:\\a\u{85}"]).is_err());
    }

    #[test]
    fn fallback_sql_vectors() {
        let s = scope_clause(&["C:\\u"]).unwrap();
        let q = fallback_sql(&["calc", "2026"], &s).unwrap();
        assert!(q.starts_with("SELECT System.ItemPathDisplay, System.FileName"));
        assert!(q.contains("FROM SystemIndex WHERE (SCOPE='file:C:\\u') AND "));
        assert!(q.contains("CONTAINS(System.FileName, '\"calc*\"') AND CONTAINS(System.FileName, '\"2026*\"')"));
        assert_eq!(fallback_sql(&["o'n"], &s).unwrap().matches("'\"o''n*\"'").count(), 1, "single quote doubled");
        assert_eq!(fallback_sql(&["a\"b"], &s), Err(Refusal::DoubleQuote));
        assert_eq!(fallback_sql(&["a\tb"], &s), Err(Refusal::ControlChar));
        assert_eq!(fallback_sql(&[], &s), Err(Refusal::Empty));
        assert_eq!(fallback_sql(&[""], &s), Err(Refusal::Empty));
        // % and -- are plain characters inside the quoted term
        let q = fallback_sql(&["50%", "a--b"], &s).unwrap();
        assert!(q.contains("'\"50%*\"'") && q.contains("'\"a--b*\"'"));
        // limits: 8 words of 64 characters pass, 9 words and 65 characters do not
        let w64 = "x".repeat(64);
        let w65 = "x".repeat(65);
        let eight: Vec<&str> = std::iter::repeat_n(w64.as_str(), 8).collect();
        assert!(fallback_sql(&eight, &s).is_ok());
        let nine: Vec<&str> = std::iter::repeat_n("a", 9).collect();
        assert_eq!(fallback_sql(&nine, &s), Err(Refusal::TooManyWords));
        assert_eq!(fallback_sql(&[w65.as_str()], &s), Err(Refusal::WordTooLong));
    }

    #[test]
    fn json_strings_escape() {
        assert_eq!(jstr("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
        assert_eq!(jstr("caffè"), "\"caffè\"");
    }

    #[test]
    fn pt_images() {
        assert!(is_pt_image("PowerToys.exe"));
        assert!(is_pt_image("PowerToys.PowerLauncher.exe"));
        assert!(is_pt_image("Microsoft.CmdPal.UI.exe"));
        assert!(!is_pt_image("notepad.exe"));
        assert!(!is_pt_image("PowerToys.dll"));
        assert_eq!(file_name_of("C:\\Program Files\\PowerToys\\PowerToys.exe"), "PowerToys.exe");
        assert_eq!(file_name_of("bare.exe"), "bare.exe");
    }

    #[test]
    fn named_links() {
        assert!(is_named_link("https://x/y.URL", false));
        assert!(is_named_link("C:\\Docs", true));
        assert!(!is_named_link("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", false));
    }

    #[test]
    fn cmdpal_scan() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"Bookmarks":[{"Name":"Docs","Url":"https://d"}],"Other":{"x":1},"Ext":{"ExtensionList":{"a":true}},"UsageCount":7}"#,
        )
        .unwrap();
        let r = scan_cmdpal(&v, 100);
        assert!(r.contains(&("Bookmarks.0.Name".to_string(), "Docs".to_string())));
        assert!(r.contains(&("Ext.ExtensionList.a".to_string(), "true".to_string())));
        assert!(r.contains(&("UsageCount".to_string(), "7".to_string())));
        assert!(!r.iter().any(|(p, _)| p.starts_with("Other")));
        assert_eq!(scan_cmdpal(&v, 1).len(), 1);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Windows side (not unit tested; exercised by the owner in L0-2).
// ---------------------------------------------------------------------------------------------------------------

#[cfg(windows)]
mod sys;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest: Vec<&str> = args.iter().skip(1).map(|s| s.as_str()).collect();
    match args.first().map(|s| s.as_str()) {
        Some("sid") => sys::cmd_sid(),
        Some("wsi") => sys::cmd_wsi(&rest),
        Some("pt") => sys::cmd_pt(),
        Some("links") => sys::cmd_links(),
        Some("fgwatch") => sys::cmd_fgwatch(&rest),
        Some("cmdpal-items") => sys::cmd_cmdpal(&rest),
        _ => {
            eprintln!(
                "launcher-probe (read-only; JSON lines on stdout)\n  sid\n  wsi [--route a|b] <words>\n  pt\n  links\n  fgwatch <seconds>\n  cmdpal-items <file>..."
            );
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("launcher-probe runs on Windows only");
}
