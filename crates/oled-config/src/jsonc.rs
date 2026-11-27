//! JSONC helpers: comment stripping, the byte order mark, key locations, and the value writers of the commented
//! templates (`js`, `js_block`, `render_extra`, as the dock's config.rs writes its file).
//!
//! Every function here is total: any text, including one a medium-integrity process wrote, gives a result and never
//! panics, and nothing allocates more than a small multiple of its input.

use serde::Serialize;

/// Removes `//` line comments and `/* */` block comments (JSONC), leaving string literals intact
/// (escaped quotes included). Newlines are kept so serde error positions still match the file.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut it = src.chars().peekable();
    let mut in_str = false;
    while let Some(c) = it.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if it.peek() == Some(&'/') => {
                while let Some(&n) = it.peek() {
                    if n == '\n' {
                        break;
                    }
                    it.next();
                }
            }
            '/' if it.peek() == Some(&'*') => {
                it.next();
                let mut prev = '\0';
                for n in it.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// `s` without a leading UTF-8 byte order mark (Notepad may write one).
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// 1-based line and column (in characters) of the opening quote of the key `json_path` in a JSONC text: `"a"` for a
/// top-level key, `"a.b"` for the key `b` of the object that is the value of `a`. The same text inside a string, a
/// comment or an array never matches; the first match wins; `None` when the key is not there.
pub fn locate(text: &str, json_path: &str) -> Option<(u32, u32)> {
    let want: Vec<&str> = json_path.split('.').collect();
    // One frame per open object or array; an object remembers the key read last and whether a key comes next.
    let mut stack: Vec<Frame> = Vec::new();
    let mut it = Chars { inner: text.chars().peekable(), line: 1, col: 0 };
    while let Some(c) = it.next() {
        match c {
            '/' if it.peek() == Some('/') => {
                while it.peek().is_some_and(|n| n != '\n') {
                    it.next();
                }
            }
            '/' if it.peek() == Some('*') => {
                it.next();
                let mut prev = '\0';
                while let Some(n) = it.next() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            '"' => {
                let at = (it.line, it.col);
                let expects_key = matches!(stack.last(), Some(Frame::Object { expect_key: true, .. }));
                if !expects_key {
                    skip_string(&mut it);
                    continue;
                }
                let key = read_string(&mut it);
                if key_matches(&stack, &want, &key) {
                    return Some(at);
                }
                if let Some(Frame::Object { key: k, expect_key }) = stack.last_mut() {
                    *k = Some(key);
                    *expect_key = false;
                }
            }
            '{' => stack.push(Frame::Object { key: None, expect_key: true }),
            '[' => stack.push(Frame::Array),
            '}' | ']' => {
                stack.pop();
            }
            ',' => {
                if let Some(Frame::Object { expect_key, .. }) = stack.last_mut() {
                    *expect_key = true;
                }
            }
            _ => {}
        }
    }
    None
}

/// An open container of `locate`'s walk.
enum Frame {
    Object { key: Option<String>, expect_key: bool },
    Array,
}

/// true when the key just read in the innermost object is `want`: every enclosing container is an object whose
/// current key is the matching segment, and the key itself is the last one.
fn key_matches(stack: &[Frame], want: &[&str], key: &str) -> bool {
    if stack.len() != want.len() || want.last() != Some(&key) {
        return false;
    }
    stack.iter().zip(want).take(stack.len().saturating_sub(1)).all(|(f, w)| match f {
        Frame::Object { key: Some(k), .. } => k == w,
        _ => false,
    })
}

/// The characters of a text with the line and column (1-based, in characters) of the one returned last.
struct Chars<'a> {
    inner: std::iter::Peekable<std::str::Chars<'a>>,
    line: u32,
    col: u32,
}

impl Chars<'_> {
    fn next(&mut self) -> Option<char> {
        let c = self.inner.next()?;
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.col = 0;
        } else {
            self.col = self.col.saturating_add(1);
        }
        Some(c)
    }

    fn peek(&mut self) -> Option<char> {
        self.inner.peek().copied()
    }
}

/// Skips the rest of a string literal (the opening quote was read).
fn skip_string(it: &mut Chars<'_>) {
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                it.next();
            }
            '"' => return,
            _ => {}
        }
    }
}

/// Reads the rest of a string literal (the opening quote was read) and decodes its escapes; a bad escape becomes
/// U+FFFD, an unterminated string ends at the end of the text.
fn read_string(it: &mut Chars<'_>) -> String {
    let mut s = String::new();
    while let Some(c) = it.next() {
        match c {
            '"' => break,
            '\\' => match it.next() {
                Some('n') => s.push('\n'),
                Some('t') => s.push('\t'),
                Some('r') => s.push('\r'),
                Some('b') => s.push('\u{8}'),
                Some('f') => s.push('\u{c}'),
                Some('u') => {
                    let mut v = 0u32;
                    for _ in 0..4 {
                        v = v * 16 + it.next().and_then(|h| h.to_digit(16)).unwrap_or(0);
                    }
                    s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                }
                Some(o) => s.push(o),
                None => break,
            },
            _ => s.push(c),
        }
    }
    s
}

/// Compact JSON of a value (`"null"` if it cannot be serialized).
pub fn js<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Pretty JSON of a collection, continuation lines indented to sit under a 2-space key.
pub fn js_block<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".to_string()).replace('\n', "\n  ")
}

/// The `extra` keys as the last lines of the file, one compact line each: "" when there are none, so
/// such a file is written as before.
pub fn render_extra(extra: &serde_json::Map<String, serde_json::Value>) -> String {
    if extra.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = extra.iter().map(|(k, v)| format!("  {}: {}", js(k), js(v))).collect();
    format!(",\n\n  // Kept from a newer version of the dock. Leave as is.\n{}", lines.join(",\n"))
}
