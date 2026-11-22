//! The basic calculator (5.4.4, L1a): + - * x × / ÷ ^ and %, parentheses, unary minus, f64, and the locale's number
//! rules. L1b adds functions, constants and literals. A recursive descent over a token vector with a depth counter.

use super::model::NumberStyle;
use super::strings;

const MAX_CHARS: usize = 256;
const MAX_TOKENS: usize = 1000;
const MAX_DEPTH: u32 = 64;
const TWO_53: f64 = 9_007_199_254_740_992.0;

/// What the answer row shows and what its verbs use.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer {
    /// The result as shown (the locale's decimal mark; "≈ " in front beyond 2^53).
    pub shown: String,
    /// The result as copied: the same digits, without the "≈ ".
    pub raw: String,
    /// The expression as the calculator read it (operators spelled one way, numbers with the locale's decimal mark).
    pub normalised: String,
    /// "1.234 = 1234 (thousands)" when a lone group mark was read as thousands.
    pub thousands_note: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Plus,
    Minus,
    Mul,
    Div,
    Pow,
    Pct,
    Open,
    Close,
}

struct Lexed {
    toks: Vec<Tok>,
    note: Option<String>,
}

/// A number literal made of digits and the two marks. Returns its value and whether a lone group mark was read as
/// thousands.
fn number(run: &str, num: NumberStyle) -> Option<(f64, bool)> {
    let is_mark = |c: char| c == num.decimal || c == num.group;
    let marks: Vec<char> = run.chars().filter(|c| is_mark(*c)).collect();
    let segs: Vec<&str> = run.split(is_mark).collect();
    if marks.is_empty() {
        return run.parse::<f64>().ok().map(|v| (v, false));
    }
    let decimals = marks.iter().filter(|c| **c == num.decimal).count();
    let groups = marks.len() - decimals;
    let three = |s: &&str| s.len() == 3;
    let lead_ok = |s: &str| (1..=3).contains(&s.len()) && !s.starts_with('0');
    if decimals > 1 {
        return None;
    }
    if decimals == 1 {
        if marks.last() != Some(&num.decimal) {
            return None;
        }
        let (frac, ints) = segs.split_last()?;
        if groups > 0 {
            let (first, rest) = ints.split_first()?;
            if !((1..=3).contains(&first.len()) && rest.iter().all(three)) {
                return None;
            }
        }
        let mut text: String = ints.concat();
        if text.is_empty() {
            text.push('0');
        }
        text.push('.');
        text.push_str(frac);
        return text.parse::<f64>().ok().map(|v| (v, false));
    }
    // Only the group mark appears.
    let (first, rest) = segs.split_first()?;
    if rest.iter().all(three) && lead_ok(first) {
        return segs.concat().parse::<f64>().ok().map(|v| (v, groups == 1));
    }
    if groups == 1 {
        let second = rest.first()?;
        let text = format!("{}.{}", if first.is_empty() { "0" } else { first }, second);
        return text.parse::<f64>().ok().map(|v| (v, false));
    }
    None
}

fn lex(text: &str, num: NumberStyle) -> Option<Lexed> {
    if text.chars().count() > MAX_CHARS {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut toks = Vec::new();
    let mut note = None;
    let mut i = 0;
    let is_mark = |c: char| c == num.decimal || c == num.group;
    while let Some(&c) = chars.get(i) {
        if toks.len() > MAX_TOKENS {
            return None;
        }
        let digit_at = |k: usize| chars.get(k).is_some_and(|d| d.is_ascii_digit());
        if c.is_ascii_digit() || (is_mark(c) && !c.is_whitespace() && digit_at(i + 1)) {
            let start = i;
            let mut end = i;
            while let Some(&d) = chars.get(i) {
                if d.is_ascii_digit() {
                    i += 1;
                    end = i;
                } else if is_mark(d) && digit_at(i + 1) {
                    i += 1;
                } else {
                    break;
                }
            }
            let run: String = chars.get(start..end)?.iter().collect();
            let (v, thousands) = number(&run, num)?;
            if thousands && note.is_none() {
                note = Some(format!("{run} = {} {}", format_plain(v, num), strings::thousands()));
            }
            toks.push(Tok::Num(v));
            continue;
        }
        i += 1;
        toks.push(match c {
            c if c.is_whitespace() => {
                continue;
            }
            '+' => Tok::Plus,
            '-' | '−' => Tok::Minus,
            '*' | 'x' | 'X' | '×' => Tok::Mul,
            '/' | '÷' => Tok::Div,
            '^' => Tok::Pow,
            '%' => Tok::Pct,
            '(' => Tok::Open,
            ')' => Tok::Close,
            _ => return None,
        });
    }
    Some(Lexed { toks, note })
}

/// A value, with the percentage it was written as when the whole operand was "x%".
#[derive(Clone, Copy)]
struct Val {
    v: f64,
    pct: Option<f64>,
}

struct Parser<'a> {
    toks: &'a [Tok],
    at: usize,
    depth: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.at).copied()
    }

    fn expr(&mut self) -> Option<Val> {
        let mut acc = self.term()?;
        while let Some(op @ (Tok::Plus | Tok::Minus)) = self.peek() {
            self.at += 1;
            let rhs = self.term()?;
            // "a + b%" and "a - b%" take b percent of a.
            let r = match rhs.pct {
                Some(p) => acc.v * p / 100.0,
                None => rhs.v,
            };
            let v = if op == Tok::Plus { acc.v + r } else { acc.v - r };
            acc = Val { v: finite(v)?, pct: None };
        }
        Some(acc)
    }

    fn term(&mut self) -> Option<Val> {
        let mut acc = self.unary()?;
        while let Some(op @ (Tok::Mul | Tok::Div)) = self.peek() {
            self.at += 1;
            let rhs = self.unary()?;
            let v = if op == Tok::Mul {
                acc.v * rhs.v
            } else {
                if rhs.v == 0.0 {
                    return None;
                }
                acc.v / rhs.v
            };
            acc = Val { v: finite(v)?, pct: None };
        }
        Some(acc)
    }

    fn unary(&mut self) -> Option<Val> {
        match self.peek() {
            Some(op @ (Tok::Minus | Tok::Plus)) => {
                self.at += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                let inner = self.unary();
                self.depth -= 1;
                let v = inner?.v;
                Some(Val { v: if op == Tok::Minus { -v } else { v }, pct: None })
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Option<Val> {
        let base = self.postfix()?;
        if self.peek() == Some(Tok::Pow) {
            self.at += 1;
            let exp = self.unary()?;
            return Some(Val { v: finite(base.v.powf(exp.v))?, pct: None });
        }
        Some(base)
    }

    fn postfix(&mut self) -> Option<Val> {
        let mut val = self.primary()?;
        while self.peek() == Some(Tok::Pct) {
            self.at += 1;
            val = Val { v: val.v / 100.0, pct: Some(val.v) };
        }
        Some(val)
    }

    fn primary(&mut self) -> Option<Val> {
        match self.peek()? {
            Tok::Num(v) => {
                self.at += 1;
                Some(Val { v, pct: None })
            }
            Tok::Open => {
                self.at += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                let inner = self.expr();
                self.depth -= 1;
                let inner = inner?;
                if self.peek() != Some(Tok::Close) {
                    return None;
                }
                self.at += 1;
                Some(Val { v: inner.v, pct: None })
            }
            _ => None,
        }
    }
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// At most 15 significant digits, trailing zeros trimmed, the locale's decimal mark, scientific notation outside
/// 1e-9..=1e15. No group marks.
fn format_plain(v: f64, num: NumberStyle) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    let sci = format!("{:.14e}", a);
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    if !(1e-9..=1e15).contains(&a) {
        let mut it = digits.chars();
        if let Some(first) = it.next() {
            out.push(first);
        }
        let rest: String = it.collect();
        if !rest.is_empty() {
            out.push(num.decimal);
            out.push_str(&rest);
        }
        out.push('e');
        out.push_str(&exp.to_string());
    } else if exp >= 0 {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            out.push_str(&digits);
            out.extend(std::iter::repeat_n('0', int_len - digits.len()));
        } else {
            out.push_str(digits.get(..int_len).unwrap_or(""));
            out.push(num.decimal);
            out.push_str(digits.get(int_len..).unwrap_or(""));
        }
    } else {
        out.push('0');
        out.push(num.decimal);
        out.extend(std::iter::repeat_n('0', (-exp - 1) as usize));
        out.push_str(&digits);
    }
    out
}

fn normalise(toks: &[Tok], num: NumberStyle) -> String {
    let mut out = String::new();
    let mut prev_value = false;
    for t in toks {
        match t {
            Tok::Num(v) => {
                out.push_str(&format_plain(*v, num));
                prev_value = true;
            }
            Tok::Plus | Tok::Minus | Tok::Mul | Tok::Div | Tok::Pow => {
                let s = match t {
                    Tok::Plus => "+",
                    Tok::Minus => "-",
                    Tok::Mul => "×",
                    Tok::Div => "÷",
                    _ => "^",
                };
                if prev_value {
                    out.push(' ');
                    out.push_str(s);
                    out.push(' ');
                } else {
                    out.push_str(s);
                }
                prev_value = false;
            }
            Tok::Pct => {
                out.push('%');
                prev_value = true;
            }
            Tok::Open => {
                out.push('(');
                prev_value = false;
            }
            Tok::Close => {
                out.push(')');
                prev_value = true;
            }
        }
    }
    out
}

/// Evaluates (a plain arithmetic parser; nothing here runs the text as code) `text` with the locale's marks. None for anything that is not a complete, finite expression
/// (never an error row while typing).
pub fn eval(text: &str, num: NumberStyle) -> Option<Answer> {
    let lexed = lex(text, num)?;
    if lexed.toks.is_empty() || lexed.toks.len() > MAX_TOKENS {
        return None;
    }
    let mut p = Parser { toks: &lexed.toks, at: 0, depth: 0 };
    let val = p.expr()?;
    if p.at != lexed.toks.len() {
        return None;
    }
    let v = finite(val.v)?;
    let raw = format_plain(v, num);
    let shown = if v.abs() >= TWO_53 { format!("≈ {raw}") } else { raw.clone() };
    Some(Answer { shown, raw, normalised: normalise(&lexed.toks, num), thousands_note: lexed.note })
}

/// True when the text parses and contains an operator or a parenthesis: a bare number never answers in Mixed mode.
pub fn is_implicit(text: &str, num: NumberStyle) -> bool {
    match lex(text, num) {
        Some(l) => {
            l.toks.iter().any(|t| matches!(t, Tok::Plus | Tok::Minus | Tok::Mul | Tok::Div | Tok::Pow | Tok::Pct))
                && eval(text, num).is_some()
        }
        None => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    const IT: NumberStyle = NumberStyle { decimal: ',', group: '.' };
    const EN: NumberStyle = NumberStyle { decimal: '.', group: ',' };

    fn shown(t: &str, n: NumberStyle) -> Option<String> {
        eval(t, n).map(|a| a.shown)
    }

    #[test]
    fn vectors_80() {
        let it: [(&str, Option<&str>); 40] = [
            ("1.234", Some("1234")),
            ("3,5", Some("3,5")),
            ("1.5", Some("1,5")),
            ("1.234,5", Some("1234,5")),
            ("1.234.567", Some("1234567")),
            ("12.34.567", None),
            ("1,2,3", None),
            ("1,234.5", None),
            ("2+", None),
            ("1/0", None),
            ("2^10", Some("1024")),
            ("50+10%", Some("55")),
            ("50-10%", Some("45")),
            ("200*10%", Some("20")),
            ("10%", Some("0,1")),
            ("-(2*3)", Some("-6")),
            ("2 x 3", Some("6")),
            ("2×3", Some("6")),
            ("9÷3", Some("3")),
            ("(1+2)*3", Some("9")),
            ("1+2*3", Some("7")),
            ("2^3^2", Some("512")),
            ("-2^2", Some("-4")),
            ("2^-1", Some("0,5")),
            ("0,1+0,2", Some("0,3")),
            ("1/3", Some("0,333333333333333")),
            ("1e5", None),
            ("abc", None),
            ("", None),
            ("()", None),
            ("(1", None),
            ("1)", None),
            ("1 2", None),
            ("0.123", Some("0,123")),
            ("10 - 4 - 3", Some("3")),
            ("100/10/2", Some("5")),
            ("1000000*1000000", Some("1000000000000")),
            ("0,000000001", Some("0,000000001")),
            ("10^20", Some("≈ 1e20")),
            ("9007199254740993", Some("≈ 9,00719925474099e15")),
        ];
        for (t, want) in it {
            assert_eq!(shown(t, IT).as_deref(), want, "it {t:?}");
        }
        let en: [(&str, Option<&str>); 40] = [
            ("1,234", Some("1234")),
            ("3,5", Some("3.5")),
            ("3.5", Some("3.5")),
            ("1,234.5", Some("1234.5")),
            ("1,234,567", Some("1234567")),
            ("1,234.567,8", None),
            ("1.234,5", None),
            ("1.2.3", None),
            ("2+", None),
            ("1/0", None),
            ("2^10", Some("1024")),
            ("50+10%", Some("55")),
            ("50-10%", Some("45")),
            ("10%", Some("0.1")),
            ("-(2*3)", Some("-6")),
            ("2 x 3", Some("6")),
            ("2 X 3", Some("6")),
            ("(2+3)%", Some("0.05")),
            ("5+(10%)", Some("5.1")),
            ("1+2*3", Some("7")),
            ("--3", Some("3")),
            ("2*-3", Some("-6")),
            (".5+.25", Some("0.75")),
            ("0.1+0.2", Some("0.3")),
            ("22/7", Some("3.14285714285714")),
            ("1/8", Some("0.125")),
            ("1e5", None),
            ("pi", None),
            ("   ", None),
            ("2 +", None),
            ("* 2", None),
            ("2 3", None),
            ("1,5,", None),
            ("0.5.", None),
            ("1,000*1,000", Some("1000000")),
            ("123456789*1000", Some("123456789000")),
            ("2^0.5", Some("1.4142135623731")),
            ("0^0", Some("1")),
            ("(-8)^0.5", None),
            ("9007199254740993", Some("≈ 9.00719925474099e15")),
        ];
        for (t, want) in en {
            assert_eq!(shown(t, EN).as_deref(), want, "en {t:?}");
        }
    }

    #[test]
    fn thousands_note_and_answer_fields() {
        let a = eval("1.234", IT).unwrap();
        assert_eq!(a.thousands_note.as_deref(), Some("1.234 = 1234 (thousands)"));
        assert_eq!(eval("1.234.567", IT).unwrap().thousands_note, None);
        assert_eq!(eval("1,234", EN).unwrap().thousands_note.as_deref(), Some("1,234 = 1234 (thousands)"));
        assert_eq!(eval("3,5", IT).unwrap().thousands_note, None);
        let a = eval("50 + 10 %", EN).unwrap();
        assert_eq!((a.shown.as_str(), a.raw.as_str(), a.normalised.as_str()), ("55", "55", "50 + 10%"));
        let a = eval("2x3", IT).unwrap();
        assert_eq!(a.normalised, "2 × 3");
        let a = eval("1/4", IT).unwrap();
        assert_eq!((a.shown.as_str(), a.raw.as_str()), ("0,25", "0,25"));
        let big = eval("9007199254740993", EN).unwrap();
        assert!(big.shown.starts_with('≈') && !big.raw.starts_with('≈'));
    }

    #[test]
    fn limits() {
        let deep = |n: usize| format!("{}1{}", "(".repeat(n), ")".repeat(n));
        assert_eq!(shown(&deep(64), EN).as_deref(), Some("1"));
        assert_eq!(shown(&deep(65), EN), None);
        assert_eq!(shown(&format!("{}1", "-".repeat(65)), EN), None);
        assert_eq!(shown(&format!("1{}", "+1".repeat(100)), EN).as_deref(), Some("101"));
        assert_eq!(shown(&"1".repeat(256), EN).as_deref(), Some("≈ 1.11111111111111e255"));
        assert_eq!(shown(&"1".repeat(257), EN), None);
    }

    #[test]
    fn implicit_needs_an_operator() {
        assert!(!is_implicit("12", EN));
        assert!(!is_implicit("(12)", EN));
        assert!(!is_implicit("1,234", EN));
        assert!(is_implicit("1+2", EN));
        assert!(is_implicit("10%", EN));
        assert!(is_implicit("-3", EN));
        assert!(!is_implicit("2+", EN));
        assert!(!is_implicit("1/0", EN));
        assert!(!is_implicit("hello", EN));
    }

    #[test]
    fn seeded_100k_never_panics() {
        let mut r = XorShift::new(0xCA1C);
        let styles = [IT, EN];
        let alphabet = ['0', '1', '2', '9', '.', ',', '+', '-', '*', '/', '^', '%', '(', ')', ' ', 'x', '×', '÷'];
        for i in 0..100_000 {
            let n = styles[i % 2];
            let t = if i % 3 == 0 {
                r.ascii_or_bmp(40)
            } else {
                let len = r.next_u64() % 40;
                (0..len).map(|_| *r.pick(&alphabet)).collect()
            };
            let _ = eval(&t, n);
            let _ = is_implicit(&t, n);
        }
    }
}
