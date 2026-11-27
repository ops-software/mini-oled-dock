//! The natural-sort corpus (test code only: the unit tests of listing/natural.rs include it with #[path], the
//! integration tests through mod common). Version 1: 50,000 WTF-8 names from the seed 0x9E37_79B9_7F4A_7C15
//! (xorshift64*), in the fixed mix of [MIX]. A change of the generator must bump the version, never edit version 1.

/// The classes of version 1, in corpus order, with the number of names of each.
pub const MIX: [(&str, usize); 13] = [
    ("ascii_words", 10_000),
    ("leading_zeros", 5_000),
    ("case_variants", 5_000),
    ("diacritics", 5_000),
    ("cjk", 4_000),
    ("dots", 5_000),
    ("spaces", 3_000),
    ("punctuation", 4_000),
    ("scripts", 5_000),
    ("long_digit_runs", 1_000),
    ("unicode_digits", 1_500),
    ("emoji", 1_000),
    ("lone_surrogates", 500),
];

const SEED_V1: u64 = 0x9E37_79B9_7F4A_7C15;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0..n
    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 11) % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    /// An inclusive range of scalar values, skipping the surrogate gap.
    fn scalar(&mut self, lo: u32, hi: u32) -> char {
        loop {
            let c = lo + (self.next() >> 20) as u32 % (hi - lo + 1);
            if let Some(ch) = char::from_u32(c) {
                return ch;
            }
        }
    }
}

const WORDS: [&str; 48] = [
    "report", "photo", "invoice", "notes", "draft", "backup", "image", "chapter", "track", "season", "episode", "scan",
    "holiday", "budget", "readme", "setup", "build", "release", "data", "config", "log", "test", "sample", "final",
    "copy", "letter", "music", "video", "page", "table", "figure", "slide", "memo", "plan", "design", "model",
    "output", "input", "result", "summary", "index", "archive", "project", "meeting", "recipe", "travel", "manual",
    "version",
];

const EXTS: [&str; 14] =
    ["txt", "png", "jpg", "rs", "md", "pdf", "docx", "zip", "tar.gz", "mp3", "json", "cfg", "bak", "exe"];

fn ext(r: &mut Rng) -> String {
    if r.chance(80) {
        format!(".{}", r.pick(&EXTS))
    } else {
        String::new()
    }
}

fn number(r: &mut Rng) -> u32 {
    match r.below(4) {
        0 => r.below(10) as u32,
        1 => r.below(100) as u32,
        2 => r.below(1_000) as u32,
        _ => r.below(100_000) as u32,
    }
}

fn ascii_word(r: &mut Rng) -> String {
    let sep = *r.pick(&["", " ", "_", " "]);
    format!("{}{}{}{}", r.pick(&WORDS), sep, number(r), ext(r))
}

fn leading_zero(r: &mut Rng) -> String {
    let zeros = "0".repeat(1 + r.below(4));
    let sep = *r.pick(&["", " ", "_"]);
    format!("{}{}{}{}{}", r.pick(&WORDS), sep, zeros, 1 + r.below(300), ext(r))
}

fn case_variant(r: &mut Rng) -> String {
    let stem = format!("{}{}", r.pick(&WORDS), if r.chance(60) { number(r) % 20 } else { 0 });
    let cased: String = match r.below(4) {
        0 => stem.to_ascii_uppercase(),
        1 => {
            let mut c = stem.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        }
        2 => stem,
        _ => stem.chars().map(|c| if r.chance(50) { c.to_ascii_uppercase() } else { c }).collect(),
    };
    format!("{cased}{}", ext(r))
}

fn diacritic(r: &mut Rng) -> String {
    const PIECES: [&str; 14] = [
        "\u{e9}", "\u{e8}", "\u{fc}", "\u{f1}", "\u{f8}", "\u{e5}", "\u{df}", "\u{e7}", "\u{142}", "\u{159}",
        "\u{17e}", "e\u{301}", "\u{c9}", "\u{d6}",
    ];
    const STEMS: [&str; 10] = ["caf", "na", "jalape", "r", "sm", "fa", "stra", "gl", "z", "pr"];
    let mut s = String::new();
    for _ in 0..1 + r.below(3) {
        let stem: &&str = r.pick(&STEMS);
        s.push_str(stem);
        let piece: &&str = r.pick(&PIECES);
        s.push_str(piece);
    }
    if r.chance(50) {
        s.push_str(&number(r).to_string());
    }
    s + &ext(r)
}

fn cjk(r: &mut Rng) -> String {
    let (lo, hi) = *r.pick(&[(0x4E00u32, 0x9FA5u32), (0x3041, 0x3096), (0x30A1, 0x30FA), (0xAC00, 0xD7A3)]);
    let mut s: String = (0..1 + r.below(6)).map(|_| r.scalar(lo, hi)).collect();
    if r.chance(40) {
        s.push_str(&number(r).to_string());
    }
    s + &ext(r)
}

fn dots(r: &mut Rng) -> String {
    let w = r.pick(&WORDS);
    match r.below(5) {
        0 => format!(".{w}{}", number(r) % 50),
        1 => format!("{w}.{}.{}", r.pick(&EXTS), r.pick(&["bak", "old", "tmp", "1", "2"])),
        2 => format!("{w}.{}.{}.{}.{}", number(r) % 9, number(r) % 9, number(r) % 9, r.pick(&EXTS)),
        3 => format!("{w}{}.{}", number(r), r.pick(&EXTS)),
        _ => format!("{}.{w}.{}", r.pick(&WORDS), number(r) % 100),
    }
}

fn spaces(r: &mut Rng) -> String {
    let (a, b) = (r.pick(&WORDS), r.pick(&WORDS));
    let run = " ".repeat(1 + r.below(4));
    match r.below(4) {
        0 => format!("{a}{run}{b}{}", number(r) % 100),
        1 => format!("{run}{a}{}", number(r) % 100),
        2 => format!("{a}{}{run}", number(r) % 100),
        _ => format!("{a} {}{run}{b}{}", number(r) % 30, ext(r)),
    }
}

fn punctuation(r: &mut Rng) -> String {
    let p = *r.pick(&["-", "'", "_", " - ", "_-_", "-'"]);
    let (a, b) = (r.pick(&WORDS), r.pick(&WORDS));
    format!("{a}{p}{b}{p}{}{}", number(r) % 200, ext(r))
}

fn script(r: &mut Rng) -> String {
    let (lo, hi) = *r.pick(&[
        (0x0410u32, 0x044F),
        (0x0391, 0x03C9),
        (0x0627, 0x064A),
        (0x05D0, 0x05EA),
        (0x0905, 0x0939),
        (0x0E01, 0x0E2E),
    ]);
    let mut s: String = (0..2 + r.below(6)).map(|_| r.scalar(lo, hi)).collect();
    if r.chance(50) {
        s.push(' ');
        s.push_str(&number(r).to_string());
    }
    s + &ext(r)
}

fn long_digits(r: &mut Rng) -> String {
    let len = 20 + r.below(21);
    let digits: String = (0..len).map(|_| char::from(b'0' + r.below(10) as u8)).collect();
    format!("{}{}{}", r.pick(&WORDS), digits, ext(r))
}

fn unicode_digits(r: &mut Rng) -> String {
    let base = *r.pick(&[0xFF10u32, 0x0660, 0x0966]);
    let mut s = String::from(*r.pick(&WORDS));
    for _ in 0..1 + r.below(2) {
        for _ in 0..1 + r.below(4) {
            s.push(char::from_u32(base + r.below(10) as u32).unwrap_or('0'));
        }
        if r.chance(50) {
            s.push('_');
        }
    }
    s + &ext(r)
}

fn emoji(r: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..1 + r.below(3) {
        s.push(r.scalar(0x1F300, 0x1FAFF));
    }
    if r.chance(50) {
        s.push_str(&format!(" {}", number(r) % 100));
    }
    s + &ext(r)
}

/// WTF-8 with one lone surrogate (ED A0..BF 80..BF) between ASCII parts.
fn lone_surrogate(r: &mut Rng) -> Vec<u8> {
    let mut v = format!("{}{}", r.pick(&WORDS), number(r) % 100).into_bytes();
    let unit = 0xD800 + r.below(0x800) as u32;
    v.extend_from_slice(&[0xE0 | (unit >> 12) as u8, 0x80 | ((unit >> 6) & 0x3F) as u8, 0x80 | (unit & 0x3F) as u8]);
    if r.chance(60) {
        v.extend_from_slice(ext(r).as_bytes());
    }
    v
}

type Gen = dyn Fn(&mut Rng) -> Vec<u8>;

/// The WTF-8 names of corpus `version`, class after class in the order of [MIX], 50,000 distinct names.
pub fn corpus(version: u16) -> Vec<Vec<u8>> {
    assert_eq!(version, 1, "only corpus version 1 exists");
    let mut r = Rng(SEED_V1);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(50_000);
    let gens: [&Gen; 13] = [
        &|r| ascii_word(r).into_bytes(),
        &|r| leading_zero(r).into_bytes(),
        &|r| case_variant(r).into_bytes(),
        &|r| diacritic(r).into_bytes(),
        &|r| cjk(r).into_bytes(),
        &|r| dots(r).into_bytes(),
        &|r| spaces(r).into_bytes(),
        &|r| punctuation(r).into_bytes(),
        &|r| script(r).into_bytes(),
        &|r| long_digits(r).into_bytes(),
        &|r| unicode_digits(r).into_bytes(),
        &|r| emoji(r).into_bytes(),
        &|r| lone_surrogate(r),
    ];
    for ((_, count), gen) in MIX.iter().zip(gens) {
        let mut made = 0;
        while made < *count {
            let name = gen(&mut r);
            if seen.insert(name.clone()) {
                out.push(name);
                made += 1;
            }
        }
    }
    out
}

/// FNV-1a 64 of the names, each followed by a NUL byte (a name never holds one).
pub fn fingerprint(names: &[Vec<u8>]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for n in names {
        for b in n.iter().chain(std::iter::once(&0u8)) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}
