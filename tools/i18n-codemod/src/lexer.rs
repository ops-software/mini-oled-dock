//! A small Rust lexer: enough to find call sites, not a parser. It skips line and block comments (nested), char
//! literals and lifetimes, reads string literals (plain and raw, with their escapes decoded) and yields identifiers,
//! numbers and single punctuation characters with their byte offsets.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Ident,
    Num,
    /// A string literal; the decoded value is in `Tok::value` (None when an escape could not be decoded).
    Str,
    Punct(char),
}

#[derive(Clone, Debug)]
pub struct Tok {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
    pub value: Option<String>,
}

impl Tok {
    pub fn is_ident(&self, src: &str, name: &str) -> bool {
        self.kind == Kind::Ident && &src[self.start..self.end] == name
    }
    pub fn is_punct(&self, c: char) -> bool {
        self.kind == Kind::Punct(c)
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The tokens of `src`, comments and char literals dropped.
pub fn lex(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == b'"' {
            i = plain_string(src, i, i, &mut out);
        } else if c == b'\'' {
            i = char_or_lifetime(b, i);
        } else if is_ident_start(c) {
            let start = i;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            let word = &src[start..i];
            // Prefixed literals: b"..", c"..", r"..", r#".."#, br"..", cr"..".
            if matches!(word, "b" | "c") && b.get(i) == Some(&b'"') {
                i = plain_string(src, start, i, &mut out);
            } else if matches!(word, "r" | "br" | "cr") && matches!(b.get(i), Some(b'"') | Some(b'#')) {
                match raw_string(src, start, i) {
                    Some((end, value)) => {
                        out.push(Tok { kind: Kind::Str, start, end, value: Some(value) });
                        i = end;
                    }
                    None => out.push(Tok { kind: Kind::Ident, start, end: i, value: None }),
                }
            } else if matches!(word, "b") && b.get(i) == Some(&b'\'') {
                i = char_or_lifetime(b, i);
            } else {
                out.push(Tok { kind: Kind::Ident, start, end: i, value: None });
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            out.push(Tok { kind: Kind::Num, start, end: i, value: None });
        } else {
            let ch = src[i..].chars().next().unwrap_or('?');
            out.push(Tok { kind: Kind::Punct(ch), start: i, end: i + ch.len_utf8(), value: None });
            i += ch.len_utf8();
        }
    }
    out
}

/// A `"..."` literal whose opening quote is at `quote` (token start at `start` for a prefix); returns the end.
fn plain_string(src: &str, start: usize, quote: usize, out: &mut Vec<Tok>) -> usize {
    let b = src.as_bytes();
    let mut i = quote + 1;
    while i < b.len() && b[i] != b'"' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    let end = (i + 1).min(b.len());
    let value = if i < b.len() { unescape(&src[quote + 1..i]) } else { None };
    out.push(Tok { kind: Kind::Str, start, end, value });
    end
}

/// A raw string starting at the `#` or quote after its prefix; (end, value) or None when it is not one.
fn raw_string(src: &str, start: usize, at: usize) -> Option<(usize, String)> {
    let _ = start;
    let b = src.as_bytes();
    let mut i = at;
    let mut hashes = 0;
    while b.get(i) == Some(&b'#') {
        hashes += 1;
        i += 1;
    }
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let body = i + 1;
    let closer = format!("\"{}", "#".repeat(hashes));
    let rel = src[body..].find(&closer)?;
    Some((body + rel + closer.len(), src[body..body + rel].to_string()))
}

/// Skips a char literal (`'a'`, `'\\n'`, `'\\u{1f}'`) or a lifetime (`'a`); returns the offset after it.
fn char_or_lifetime(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    if b.get(j) == Some(&b'\\') {
        j += 2;
        while j < b.len() && b[j] != b'\'' && b[j] != b'\n' {
            j += 1;
        }
        return (j + 1).min(b.len());
    }
    // One (possibly multi-byte) character then a closing quote is a char literal; otherwise a lifetime.
    let mut k = j + 1;
    while k < b.len() && (b[k] & 0xC0) == 0x80 {
        k += 1;
    }
    if b.get(k) == Some(&b'\'') && j < b.len() {
        return k + 1;
    }
    while j < b.len() && is_ident_char(b[j]) {
        j += 1;
    }
    j
}

/// Decodes the escapes of a Rust string literal body; None for an escape that is not understood.
pub fn unescape(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = body.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'x' => {
                let hex: String = (0..2).filter_map(|_| it.next()).collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            'u' => {
                if it.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    match it.next()? {
                        '}' => break,
                        '_' => {}
                        h => hex.push(h),
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            '\n' | '\r' => {
                // A line continuation: the newline and the leading white space of the next line are dropped.
                while matches!(it.peek(), Some(c) if c.is_whitespace()) {
                    it.next();
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The 1-based line of a byte offset.
pub fn line_of(src: &str, offset: usize) -> usize {
    src.as_bytes()[..offset.min(src.len())].iter().filter(|&&c| c == b'\n').count() + 1
}
