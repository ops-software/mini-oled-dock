//! The strings files (strings/en.toml, strings/it.toml): a strict subset, the same grammar the build-time generator
//! accepts. Comment lines directly above an entry travel with it; the comment block at the top is the header.

use std::fmt::Write as _;

/// Written in the header of both files by the first `apply`: from then on a pair found in several areas no longer
/// becomes a `common.*` key (the common keys are decided once).
pub const FIRST_RUN_MARKER: &str = "# i18n-codemod: common keys decided";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub text: String,
    /// Comment lines directly above the entry.
    pub pre: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Toml {
    pub header: Vec<String>,
    pub entries: Vec<Entry>,
    pub tail: Vec<String>,
    /// "\r\n" when the file used it.
    pub crlf: bool,
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn unescape(body: &str, line: usize) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let cp = u32::from_str_radix(&hex, 16).map_err(|_| format!("line {line}: bad \\u escape"))?;
                out.push(char::from_u32(cp).ok_or_else(|| format!("line {line}: bad \\u escape"))?);
            }
            _ => return Err(format!("line {line}: unknown escape")),
        }
    }
    Ok(out)
}

/// Splits `"key" = "text"` (a trailing comment is not kept) into the key and the decoded text.
fn parse_entry(line: &str, n: usize) -> Result<(String, String), String> {
    let rest = line.strip_prefix('"').ok_or_else(|| format!("line {n}: a line must be \"key\" = \"text\""))?;
    let close = rest.find('"').ok_or_else(|| format!("line {n}: unterminated key"))?;
    let key = rest[..close].to_string();
    let rest = rest[close + 1..].trim_start();
    let rest = rest.strip_prefix('=').ok_or_else(|| format!("line {n}: missing ="))?.trim_start();
    let body = rest.strip_prefix('"').ok_or_else(|| format!("line {n}: the text must start with a quote"))?;
    let mut end = None;
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            end = Some(i);
            break;
        }
    }
    let end = end.ok_or_else(|| format!("line {n}: unterminated string"))?;
    let tail = body[end + 1..].trim();
    if !tail.is_empty() && !tail.starts_with('#') {
        return Err(format!("line {n}: text after the closing quote"));
    }
    Ok((key, unescape(&body[..end], n)?))
}

impl Toml {
    pub fn parse(text: &str) -> Result<Toml, String> {
        let mut t = Toml { crlf: text.contains("\r\n"), ..Toml::default() };
        let mut pending: Vec<String> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line == "=======" {
                return Err(format!("line {}: a conflict marker is left in the file", i + 1));
            }
            if line.is_empty() {
                if t.entries.is_empty() {
                    t.header.append(&mut pending);
                }
                continue;
            }
            if line.starts_with('#') {
                pending.push(line.to_string());
                continue;
            }
            let (key, text) = parse_entry(line, i + 1)?;
            if t.entries.is_empty() {
                t.header.append(&mut pending);
                // Comments glued to the first entry belong to the header (they describe the whole file).
            }
            t.entries.push(Entry { key, text, pre: std::mem::take(&mut pending) });
        }
        t.tail = pending;
        Ok(t)
    }

    pub fn has_marker(&self) -> bool {
        self.header.iter().any(|l| l == FIRST_RUN_MARKER)
    }

    pub fn add_marker(&mut self) {
        if !self.has_marker() {
            self.header.push(FIRST_RUN_MARKER.to_string());
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.key == key).map(|e| e.text.as_str())
    }

    pub fn sort(&mut self) {
        self.entries.sort_by(|a, b| a.key.cmp(&b.key));
    }

    pub fn render(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut out = String::new();
        for l in &self.header {
            out.push_str(l);
            out.push_str(eol);
        }
        if !self.header.is_empty() && !self.entries.is_empty() {
            out.push_str(eol);
        }
        for e in &self.entries {
            for c in &e.pre {
                out.push_str(c);
                out.push_str(eol);
            }
            let _ = write!(out, "\"{}\" = \"{}\"{eol}", e.key, escape(&e.text));
        }
        for l in &self.tail {
            out.push_str(l);
            out.push_str(eol);
        }
        out
    }
}

/// The constant name of a key, by the generator's rule: upper case, dots to underscores.
pub fn const_name(key: &str) -> String {
    key.to_ascii_uppercase().replace('.', "_")
}
