//! The run history file (5.4.9, 5.5.2): "mini-oled-dock launcher-run 1" and at most 50 lines of at most 256 characters,
//! at most 64 KiB. The parser is strict: one bad line, another version or a missing final newline gives an empty
//! history. Reading and writing the file is the shell thread's job (L1a-12); this module only turns bytes into lines.

pub const HEADER: &str = "mini-oled-dock launcher-run 1";
pub const MAX_LINES: usize = 50;
pub const MAX_LINE_CHARS: usize = 256;
pub const MAX_BYTES: usize = 64 * 1024;

/// A line may be stored when it is not empty, has no leading space, no control character (C0, DEL, C1) and no bidi
/// marker (U+200E, U+200F, U+202A-U+202E, U+2066-U+2069), and is at most 256 characters long.
pub fn accept(line: &str) -> bool {
    if line.is_empty() || line.starts_with(' ') || line.chars().count() > MAX_LINE_CHARS {
        return false;
    }
    !line.chars().any(char::is_control) && !has_marker(line)
}

/// True when the text holds a bidi marker (U+200E, U+200F, U+202A-U+202E, U+2066-U+2069).
pub fn has_marker(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
}

/// The lines of a history file, newest first. Empty for anything that is not exactly the format.
pub fn parse_run(bytes: &[u8]) -> Vec<Box<str>> {
    parse(bytes).unwrap_or_default()
}

fn parse(bytes: &[u8]) -> Option<Vec<Box<str>>> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let body = text.strip_prefix(HEADER)?.strip_prefix('\n')?;
    if body.is_empty() {
        return Some(Vec::new());
    }
    let body = body.strip_suffix('\n')?;
    let mut out = Vec::new();
    for line in body.split('\n') {
        if out.len() >= MAX_LINES || !accept(line) {
            return None;
        }
        out.push(Box::from(line));
    }
    Some(out)
}

/// The bytes of a history file for these lines (newest first); lines that cannot be stored are skipped.
pub fn render_run(lines: &[Box<str>]) -> Vec<u8> {
    let mut s = String::with_capacity(HEADER.len() + 1 + lines.len().min(MAX_LINES) * 32);
    s.push_str(HEADER);
    s.push('\n');
    for line in lines.iter().filter(|l| accept(l)).take(MAX_LINES) {
        s.push_str(line);
        s.push('\n');
    }
    s.into_bytes()
}

/// Puts a line first, removing an earlier copy, and keeps 50. False (nothing changed) when the line cannot be stored.
pub fn push_front(lines: &mut Vec<Box<str>>, line: &str) -> bool {
    if !accept(line) {
        return false;
    }
    lines.retain(|l| &**l != line);
    lines.insert(0, Box::from(line));
    lines.truncate(MAX_LINES);
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    fn lines(items: &[&str]) -> Vec<Box<str>> {
        items.iter().map(|s| Box::from(*s)).collect()
    }

    #[test]
    fn round_trip() {
        let l = lines(&["ping 1.1.1.1", "\"C:\\Program Files\\x.exe\" -y", "città è"]);
        let bytes = render_run(&l);
        assert!(bytes.starts_with(b"mini-oled-dock launcher-run 1\n"));
        assert_eq!(parse_run(&bytes), l);
        assert_eq!(parse_run(&render_run(&[])), Vec::<Box<str>>::new());
        let fifty: Vec<Box<str>> = (0..50).map(|i| Box::from(format!("cmd {i}"))).collect();
        assert_eq!(parse_run(&render_run(&fifty)), fifty);
        // More than 50 are cut when written.
        let sixty: Vec<Box<str>> = (0..60).map(|i| Box::from(format!("cmd {i}"))).collect();
        assert_eq!(parse_run(&render_run(&sixty)).len(), 50);
    }

    #[test]
    fn rejects_bad_version() {
        for bad in [
            "mini-oled-dock launcher-run 2\nping\n",
            "mini-oled-dock launcher-run 1\r\nping\r\n",
            "mini-oled-dock launcher-run 1 \nping\n",
            "Mini-oled-dock launcher-run 1\nping\n",
            "ping\n",
            "",
            "mini-oled-dock launcher-run 1",
            "mini-oled-dock launcher-run 1\nping",
            "\u{feff}mini-oled-dock launcher-run 1\nping\n",
            "mini-oled-dock launcher-run 1\nping\n\nnotepad\n",
            "mini-oled-dock launcher-run 1\nping\n\n",
            "mini-oled-dock launcher-run 1\n ping\n",
            "mini-oled-dock launcher-run 1\nok\nbad\u{7}\n",
        ] {
            assert!(parse_run(bad.as_bytes()).is_empty(), "{bad:?}");
        }
        assert!(parse_run(b"mini-oled-dock launcher-run 1\nping\xff\n").is_empty());
        assert_eq!(parse_run(b"mini-oled-dock launcher-run 1\nping\n"), lines(&["ping"]));
    }

    #[test]
    fn rejects_oversize() {
        let header = format!("{HEADER}\n");
        let long = "x".repeat(257);
        assert!(parse_run(format!("{header}{long}\n").as_bytes()).is_empty());
        let ok = "x".repeat(256);
        assert_eq!(parse_run(format!("{header}{ok}\n").as_bytes()).len(), 1);
        let many: String = (0..51).map(|i| format!("c{i}\n")).collect();
        assert!(parse_run(format!("{header}{many}").as_bytes()).is_empty());
        let many: String = (0..50).map(|i| format!("c{i}\n")).collect();
        assert_eq!(parse_run(format!("{header}{many}").as_bytes()).len(), 50);
        let mut big = header.into_bytes();
        big.resize(MAX_BYTES + 1, b'\n');
        assert!(parse_run(&big).is_empty());
        // Written output never exceeds the cap, even with 4-byte characters.
        let wide: Vec<Box<str>> = (0..60).map(|_| Box::from("😀".repeat(256))).collect();
        assert!(render_run(&wide).len() <= MAX_BYTES);
    }

    #[test]
    fn leading_space_and_markers_never_stored() {
        assert!(accept("ping"));
        assert!(accept("a b "));
        assert!(!accept(""));
        assert!(!accept(" ping"));
        assert!(!accept("pi\u{0}ng"));
        assert!(!accept("pi\nng"));
        assert!(!accept("pi\tng"));
        assert!(!accept("pi\u{7f}ng"));
        assert!(!accept("pi\u{85}ng"));
        for c in [
            '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}',
        ] {
            assert!(!accept(&format!("a{c}b")), "{c:?}");
        }
        let mut l = lines(&["a"]);
        assert!(!push_front(&mut l, " b"));
        assert!(!push_front(&mut l, "b\u{202E}"));
        assert_eq!(l, lines(&["a"]));
        assert!(render_run(&lines(&["ok", " bad", "x\u{2066}"])) == render_run(&lines(&["ok"])));
    }

    #[test]
    fn push_front_dedups_and_keeps_fifty() {
        let mut l = lines(&["a", "b", "c"]);
        assert!(push_front(&mut l, "b"));
        assert_eq!(l, lines(&["b", "a", "c"]));
        for i in 0..70 {
            push_front(&mut l, &format!("n{i}"));
        }
        assert_eq!(l.len(), 50);
        assert_eq!(&*l[0], "n69");
    }

    #[test]
    fn seeded_100k_parser_never_panics() {
        let mut r = XorShift::new(0x4157);
        let atoms: [&[u8]; 9] =
            [HEADER.as_bytes(), b"\n", b"\r\n", b"ping", b" ", b"\xff", "è".as_bytes(), b"\x07", "\u{202e}".as_bytes()];
        for i in 0..100_000 {
            let mut bytes: Vec<u8> = Vec::new();
            if i % 2 == 0 {
                bytes.extend_from_slice(HEADER.as_bytes());
                bytes.push(b'\n');
            }
            let n = r.next_u64() % 12;
            for _ in 0..n {
                let piece = *r.pick(&atoms);
                bytes.extend_from_slice(piece);
            }
            let parsed = parse_run(&bytes);
            // Whatever is accepted is exactly what render writes back.
            if !parsed.is_empty() {
                assert_eq!(render_run(&parsed), bytes);
            }
        }
    }
}
