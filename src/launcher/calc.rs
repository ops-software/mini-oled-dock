//! The calculator (5.4.4): + - * x × / ÷ ^ % and !, parentheses, unary minus, implicit multiplication ("2pi", "2(3)"),
//! the constants pi and e, the functions sqrt abs round floor ceil ln log log2 sin cos tan min max (radians, "°" for
//! degrees), the literals 0x 0b 0o and 1e5, "in hex|bin|dec|oct", f64 and the locale's number rules. Arguments are
//! separated by ";" in every locale and also by "," when the decimal mark is ".". A recursive descent over a token
//! vector with a depth counter; nothing here runs the text as code.

use super::model::NumberStyle;
use super::strings;

const MAX_CHARS: usize = 256;
const MAX_TOKENS: usize = 1000;
const MAX_DEPTH: u32 = 64;
const MAX_ARGS: usize = 32;
/// The largest factorial a double holds: 171! is infinite.
const MAX_FACTORIAL: f64 = 170.0;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Func {
    Sqrt,
    Abs,
    Round,
    Floor,
    Ceil,
    Ln,
    Log,
    Log2,
    Sin,
    Cos,
    Tan,
    Min,
    Max,
}

impl Func {
    fn name(self) -> &'static str {
        match self {
            Func::Sqrt => "sqrt",
            Func::Abs => "abs",
            Func::Round => "round",
            Func::Floor => "floor",
            Func::Ceil => "ceil",
            Func::Ln => "ln",
            Func::Log => "log",
            Func::Log2 => "log2",
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
            Func::Min => "min",
            Func::Max => "max",
        }
    }

    fn from_word(w: &str) -> Option<Func> {
        Some(match w {
            "sqrt" => Func::Sqrt,
            "abs" => Func::Abs,
            "round" => Func::Round,
            "floor" => Func::Floor,
            "ceil" => Func::Ceil,
            "ln" => Func::Ln,
            "log" => Func::Log,
            "log2" => Func::Log2,
            "sin" => Func::Sin,
            "cos" => Func::Cos,
            "tan" => Func::Tan,
            "min" => Func::Min,
            "max" => Func::Max,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Base {
    Hex,
    Bin,
    Dec,
    Oct,
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
    Fact,
    Deg,
    Open,
    Close,
    Semi,
    Pi,
    E,
    Func(Func),
    /// "in", only valid in front of a base at the very end.
    In,
    Base(Base),
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
            if !(lead_ok(first) && rest.iter().all(three)) {
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

/// The digits of a 0x, 0b or 0o literal starting at `at` (the "0"), with its value; None when no digit follows the
/// prefix (then "0x" is "0 x"), and a value of its own error when it does not fit in 64 bits.
fn radix_literal(chars: &[char], at: usize) -> Option<Option<(f64, usize)>> {
    let radix = match chars.get(at + 1)? {
        'x' | 'X' => 16,
        'b' | 'B' => 2,
        'o' | 'O' => 8,
        _ => return None,
    };
    let start = at + 2;
    let mut end = start;
    while chars.get(end).is_some_and(|d| d.is_digit(radix)) {
        end += 1;
    }
    if end == start {
        return None;
    }
    // A digit of another base right after the run ("0b12", "0xfg") is not a literal.
    if chars.get(end).is_some_and(|d| d.is_alphanumeric()) {
        return Some(None);
    }
    let digits: String = chars.get(start..end)?.iter().collect();
    Some(u64::from_str_radix(&digits, radix).ok().map(|v| (v as f64, end)))
}

/// "e5", "E-3" right after a number: the exponent and where it ends. "2e" and "2e+x" are not exponents ("2 times e").
fn exponent(chars: &[char], at: usize) -> Option<(i32, usize)> {
    if !matches!(chars.get(at)?, 'e' | 'E') {
        return None;
    }
    let mut i = at + 1;
    let neg = match chars.get(i) {
        Some('+') => {
            i += 1;
            false
        }
        Some('-') => {
            i += 1;
            true
        }
        _ => false,
    };
    let start = i;
    while chars.get(i).is_some_and(|d| d.is_ascii_digit()) {
        i += 1;
    }
    if i == start || i - start > 4 || chars.get(i).is_some_and(|d| d.is_alphabetic()) {
        return None;
    }
    let digits: String = chars.get(start..i)?.iter().collect();
    let e: i32 = digits.parse().ok()?;
    Some((if neg { -e } else { e }, i))
}

/// `v` times ten to the `e`, rounded once for the powers that are exact in a double.
fn scale10(v: f64, e: i32) -> f64 {
    if (0..=22).contains(&e) {
        v * 10f64.powi(e)
    } else if (-22..0).contains(&e) {
        v / 10f64.powi(-e)
    } else {
        v * 10f64.powi(e)
    }
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
        if c == '0' {
            if let Some(lit) = radix_literal(&chars, i) {
                let (v, end) = lit?;
                toks.push(Tok::Num(v));
                i = end;
                continue;
            }
        }
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
            let (mut v, thousands) = number(&run, num)?;
            if thousands && note.is_none() {
                note = Some(format!("{run} = {} {}", format_plain(v, num), strings::thousands()));
            }
            if let Some((e, after)) = exponent(&chars, end) {
                v = scale10(v, e);
                i = after;
            }
            toks.push(Tok::Num(v));
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while chars.get(i).is_some_and(|d| d.is_alphabetic()) {
                i += 1;
            }
            // "log2" is the one name with a digit.
            if chars.get(start..i).is_some_and(|w| w.iter().collect::<String>().eq_ignore_ascii_case("log"))
                && chars.get(i) == Some(&'2')
                && !digit_at(i + 1)
            {
                i += 1;
            }
            let word: String = chars.get(start..i)?.iter().collect::<String>().to_lowercase();
            toks.push(match word.as_str() {
                "x" => Tok::Mul,
                "pi" | "π" => Tok::Pi,
                "e" => Tok::E,
                "in" => Tok::In,
                "hex" => Tok::Base(Base::Hex),
                "bin" => Tok::Base(Base::Bin),
                "dec" => Tok::Base(Base::Dec),
                "oct" => Tok::Base(Base::Oct),
                w => Tok::Func(Func::from_word(w)?),
            });
            continue;
        }
        i += 1;
        toks.push(match c {
            c if c.is_whitespace() => {
                continue;
            }
            '+' => Tok::Plus,
            '-' | '−' => Tok::Minus,
            '*' | '×' => Tok::Mul,
            '/' | '÷' => Tok::Div,
            '^' => Tok::Pow,
            '%' => Tok::Pct,
            '!' => Tok::Fact,
            '°' => Tok::Deg,
            '(' => Tok::Open,
            ')' => Tok::Close,
            ';' => Tok::Semi,
            // Where the comma is not a number mark's neighbour in a "." locale it separates arguments: "max(1, 2)".
            ',' if num.decimal == '.' => Tok::Semi,
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
        // Multiplication, division, and the implicit product: "2pi", "2(3)", "(1+2)(3+4)", "2 sqrt(4)" (never a bare
        // number after a value: "2 3" is not 6).
        while let Some(op @ (Tok::Mul | Tok::Div | Tok::Open | Tok::Pi | Tok::E | Tok::Func(_))) = self.peek() {
            let explicit = matches!(op, Tok::Mul | Tok::Div);
            if explicit {
                self.at += 1;
            }
            let rhs = self.unary()?;
            let v = if op != Tok::Div {
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
        while let Some(op @ (Tok::Pct | Tok::Fact | Tok::Deg)) = self.peek() {
            self.at += 1;
            val = match op {
                Tok::Pct => Val { v: val.v / 100.0, pct: Some(val.v) },
                Tok::Fact => Val { v: factorial(val.v)?, pct: None },
                _ => Val { v: finite(val.v * (std::f64::consts::PI / 180.0))?, pct: None },
            };
        }
        Some(val)
    }

    fn primary(&mut self) -> Option<Val> {
        match self.peek()? {
            Tok::Num(v) => {
                self.at += 1;
                Some(Val { v, pct: None })
            }
            Tok::Pi => {
                self.at += 1;
                Some(Val { v: std::f64::consts::PI, pct: None })
            }
            Tok::E => {
                self.at += 1;
                Some(Val { v: std::f64::consts::E, pct: None })
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
            Tok::Func(f) => {
                self.at += 1;
                if self.peek() != Some(Tok::Open) {
                    return None;
                }
                self.at += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                let v = self.call(f);
                self.depth -= 1;
                Some(Val { v: finite(v?)?, pct: None })
            }
            _ => None,
        }
    }

    /// The arguments of `f` up to the closing parenthesis (the opening one is taken), and its value.
    fn call(&mut self, f: Func) -> Option<f64> {
        let mut args: Vec<f64> = Vec::new();
        loop {
            args.push(self.expr()?.v);
            if args.len() > MAX_ARGS {
                return None;
            }
            match self.peek() {
                Some(Tok::Semi) => self.at += 1,
                Some(Tok::Close) => {
                    self.at += 1;
                    break;
                }
                _ => return None,
            }
        }
        apply(f, &args)
    }
}

/// n! for a whole n from 0 to 170.
fn factorial(v: f64) -> Option<f64> {
    if !(0.0..=MAX_FACTORIAL).contains(&v) || v.fract() != 0.0 {
        return None;
    }
    finite((2..=v as u32).fold(1.0, |acc, i| acc * f64::from(i)))
}

/// A trigonometric result that is zero but for the noise of pi (sin(pi) is 1.2e-16) shows as 0.
fn snap(v: f64) -> f64 {
    if v.abs() < 1e-15 {
        0.0
    } else {
        v
    }
}

/// The value of a function; None outside its domain or with the wrong number of arguments.
fn apply(f: Func, args: &[f64]) -> Option<f64> {
    let v = match (f, args) {
        (Func::Sqrt, [x]) if *x >= 0.0 => x.sqrt(),
        (Func::Abs, [x]) => x.abs(),
        (Func::Round, [x]) => x.round(),
        (Func::Round, [x, n]) if n.fract() == 0.0 && (0.0..=15.0).contains(n) => {
            let scale = 10f64.powi(*n as i32);
            (x * scale).round() / scale
        }
        (Func::Floor, [x]) => x.floor(),
        (Func::Ceil, [x]) => x.ceil(),
        (Func::Ln, [x]) if *x > 0.0 => x.ln(),
        (Func::Log, [x]) if *x > 0.0 => x.log10(),
        (Func::Log2, [x]) if *x > 0.0 => x.log2(),
        (Func::Sin, [x]) => snap(x.sin()),
        (Func::Cos, [x]) => snap(x.cos()),
        (Func::Tan, [x]) => snap(x.tan()),
        (Func::Min, [first, rest @ ..]) => rest.iter().fold(*first, |a, b| a.min(*b)),
        (Func::Max, [first, rest @ ..]) => rest.iter().fold(*first, |a, b| a.max(*b)),
        _ => return None,
    };
    finite(v)
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

/// True after a token that ends a value: a product with the next one, if it starts one, is implicit.
fn ends_value(t: Tok) -> bool {
    matches!(t, Tok::Num(_) | Tok::Pct | Tok::Fact | Tok::Deg | Tok::Close | Tok::Pi | Tok::E)
}

fn normalise(toks: &[Tok], num: NumberStyle) -> String {
    let mut out = String::new();
    let mut prev: Option<Tok> = None;
    for t in toks {
        let prev_value = prev.is_some_and(ends_value);
        // "2pi" and "2(3)" are read as products, and shown as such.
        if prev_value && matches!(t, Tok::Pi | Tok::E | Tok::Func(_) | Tok::Open) {
            out.push_str(" × ");
        }
        match t {
            Tok::Num(v) => out.push_str(&format_plain(*v, num)),
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
            }
            Tok::Pct => out.push('%'),
            Tok::Fact => out.push('!'),
            Tok::Deg => out.push('°'),
            Tok::Open => out.push('('),
            Tok::Close => out.push(')'),
            Tok::Semi => out.push_str("; "),
            Tok::Pi => out.push('π'),
            Tok::E => out.push('e'),
            Tok::Func(f) => out.push_str(f.name()),
            Tok::In => out.push_str(" in "),
            Tok::Base(b) => out.push_str(match b {
                Base::Hex => "hex",
                Base::Bin => "bin",
                Base::Dec => "dec",
                Base::Oct => "oct",
            }),
        }
        prev = Some(*t);
    }
    out
}

/// A whole number of at most 2^53 in another base: "0xFF", "0b1010", "0o17", with a sign when negative. None for a
/// fraction or a value a double no longer holds exactly.
fn format_base(v: f64, base: Base) -> Option<String> {
    if v.fract() != 0.0 || v.abs() > TWO_53 {
        return None;
    }
    let n = v.abs() as u64;
    let sign = if v < 0.0 { "-" } else { "" };
    Some(match base {
        Base::Hex => format!("{sign}0x{n:X}"),
        Base::Bin => format!("{sign}0b{n:b}"),
        Base::Oct => format!("{sign}0o{n:o}"),
        Base::Dec => return None,
    })
}

/// Evaluates `text` with the locale's marks. None for anything that is not a complete, finite expression (never an error
/// row while typing).
pub fn eval(text: &str, num: NumberStyle) -> Option<Answer> {
    let lexed = lex(text, num)?;
    if lexed.toks.is_empty() || lexed.toks.len() > MAX_TOKENS {
        return None;
    }
    let mut p = Parser { toks: &lexed.toks, at: 0, depth: 0 };
    let val = p.expr()?;
    // "in hex" and its kin close the expression.
    let base = match (p.peek(), lexed.toks.get(p.at + 1)) {
        (Some(Tok::In), Some(Tok::Base(b))) => {
            p.at += 2;
            Some(*b)
        }
        _ => None,
    };
    if p.at != lexed.toks.len() {
        return None;
    }
    let v = finite(val.v)?;
    let as_base = match base {
        Some(Base::Dec) | None => None,
        Some(b) => Some(format_base(v, b)?),
    };
    if let Some(text) = as_base {
        return Some(Answer { shown: text.clone(), raw: text, normalised: normalise(&lexed.toks, num), thousands_note: lexed.note });
    }
    let raw = format_plain(v, num);
    let shown = if v.abs() >= TWO_53 { format!("≈ {raw}") } else { raw.clone() };
    Some(Answer { shown, raw, normalised: normalise(&lexed.toks, num), thousands_note: lexed.note })
}

/// True when the text parses and contains an operator or a parenthesis: a bare number never answers in Mixed mode.
pub fn is_implicit(text: &str, num: NumberStyle) -> bool {
    match lex(text, num) {
        Some(l) => {
            // A lone constant or number is a word or a number, not a sum: "e" and "pi" never answer on their own.
            let sum = l.toks.iter().any(|t| {
                matches!(
                    t,
                    Tok::Plus
                        | Tok::Minus
                        | Tok::Mul
                        | Tok::Div
                        | Tok::Pow
                        | Tok::Pct
                        | Tok::Fact
                        | Tok::Deg
                        | Tok::Func(_)
                        | Tok::In
                ) || (l.toks.len() > 1 && matches!(t, Tok::Pi | Tok::E))
            });
            // A value followed by "(", a constant or a function is a product: "2(3)", "2pi".
            let product = l.toks.windows(2).any(|w| match w {
                [a, b] => ends_value(*a) && matches!(b, Tok::Pi | Tok::E | Tok::Func(_) | Tok::Open),
                _ => false,
            });
            (sum || product) && eval(text, num).is_some()
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
    fn vectors_150() {
        let it: &[(&str, Option<&str>)] = &[
            ("0.123,5", None),
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
            ("1e5", Some("100000")),
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
            assert_eq!(shown(t, IT).as_deref(), *want, "it {t:?}");
        }
        let en: &[(&str, Option<&str>)] = &[
            ("0,123.5", None),
            ("00,123.5", None),
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
            ("1e5", Some("100000")),
            ("pi", Some("3.14159265358979")),
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
            assert_eq!(shown(t, EN).as_deref(), *want, "en {t:?}");
        }
        let it_new: &[(&str, Option<&str>)] = &[
            // Constants and implicit products.
            ("pi", Some("3,14159265358979")),
            ("π", Some("3,14159265358979")),
            ("PI", Some("3,14159265358979")),
            ("e", Some("2,71828182845905")),
            ("2pi", Some("6,28318530717959")),
            ("2 pi", Some("6,28318530717959")),
            ("pi(2)", Some("6,28318530717959")),
            ("(2)pi", Some("6,28318530717959")),
            ("pi pi", Some("9,86960440108936")),
            ("1/2pi", Some("1,5707963267949")),
            ("e^2", Some("7,38905609893065")),
            ("2(3)", Some("6")),
            ("(2)(3)", Some("6")),
            ("(1+2)(3+4)", Some("21")),
            ("2 sqrt(16)", Some("8")),
            ("pi 2", None),
            ("(2)3", None),
            ("2pi3", None),
            // Functions.
            ("sqrt(16)", Some("4")),
            ("SQRT(16)", Some("4")),
            ("sqrt(2)", Some("1,4142135623731")),
            ("sqrt(-1)", None),
            ("sqrt16", None),
            ("sqrt", None),
            ("sqrt(16", None),
            ("foo(1)", None),
            ("asin(1)", None),
            ("abs(-5)", Some("5")),
            ("abs(-2,5)", Some("2,5")),
            ("round(2,5)", Some("3")),
            ("round(-2,5)", Some("-3")),
            ("round(2,456;2)", Some("2,46")),
            ("round(2,55;1)", Some("2,6")),
            ("round(2,5;0)", Some("3")),
            ("round(1;16)", None),
            ("round(1;-1)", None),
            ("round(1;1,5)", None),
            ("round(1;2;3)", None),
            ("floor(2,9)", Some("2")),
            ("floor(-2,1)", Some("-3")),
            ("ceil(2,1)", Some("3")),
            ("ceil(-2,9)", Some("-2")),
            ("floor(1;2)", None),
            ("ln(1)", Some("0")),
            ("ln(e)", Some("1")),
            ("ln(0)", None),
            ("ln(-1)", None),
            ("log(100)", Some("2")),
            ("log(1000)", Some("3")),
            ("log(0)", None),
            ("log2(8)", Some("3")),
            ("log2(1024)", Some("10")),
            ("log2(0)", None),
            ("sin(0)", Some("0")),
            ("sin(pi)", Some("0")),
            ("sin(pi/2)", Some("1")),
            ("cos(0)", Some("1")),
            ("cos(pi)", Some("-1")),
            ("tan(0)", Some("0")),
            ("min(3;1;2)", Some("1")),
            ("max(3;1;2)", Some("3")),
            ("min(5)", Some("5")),
            ("max(1,5;2)", Some("2")),
            ("max(1,2;3)", Some("3")),
            ("max(1;2;3;4;5;6;7;8;9;10)", Some("10")),
            ("min()", None),
            ("min(1;)", None),
            ("min(1 2)", None),
            ("min(1,2)", Some("1,2")),
            // Degrees.
            ("180°", Some("3,14159265358979")),
            ("90°", Some("1,5707963267949")),
            ("sin(30°)", Some("0,5")),
            ("cos(60°)", Some("0,5")),
            ("sin(90°)", Some("1")),
            ("cos(90°)", Some("0")),
            ("tan(45°)", Some("1")),
            // Factorial.
            ("3!", Some("6")),
            ("0!", Some("1")),
            ("5!", Some("120")),
            ("10!", Some("3628800")),
            ("-3!", Some("-6")),
            ("2^3!", Some("64")),
            ("3!+1", Some("7")),
            ("2*3!", Some("12")),
            ("3!!", Some("720")),
            ("4!/3!", Some("4")),
            ("2!^2", Some("4")),
            ("3,5!", None),
            ("(-3)!", None),
            ("!", None),
            // Literals and bases.
            ("0xFF", Some("255")),
            ("0XfF", Some("255")),
            ("0x1F", Some("31")),
            ("0b101", Some("5")),
            ("0o17", Some("15")),
            ("0x10+1", Some("17")),
            ("0b11*0b11", Some("9")),
            ("0b12", None),
            ("0xG", None),
            ("0x", None),
            ("0xFFFFFFFFFFFFFFFFF", None),
            ("255 in hex", Some("0xFF")),
            ("255 in bin", Some("0b11111111")),
            ("255 in oct", Some("0o377")),
            ("255 in dec", Some("255")),
            ("0xFF in dec", Some("255")),
            ("0xFF + 1 in hex", Some("0x100")),
            ("-255 in hex", Some("-0xFF")),
            ("2^53 in hex", Some("0x20000000000000")),
            ("2^54 in hex", None),
            ("1,5 in hex", None),
            ("255 in", None),
            ("in hex", None),
            ("255 in hex hex", None),
            ("255 in foo", None),
            ("(255 in hex)", None),
            // Exponents.
            ("1,5e3", Some("1500")),
            ("2e-3", Some("0,002")),
            ("1e-9", Some("0,000000001")),
            ("1E3", Some("1000")),
            ("2e+3", Some("2000")),
            ("2e", Some("5,43656365691809")),
            ("1e5000", None),
            ("1e99999", None),
        ];
        for (t, want) in it_new {
            assert_eq!(shown(t, IT).as_deref(), *want, "it {t:?}");
        }
        let en_new: &[(&str, Option<&str>)] = &[
            ("max(1, 2)", Some("2")),
            ("max(1,2)", Some("1.2")),
            ("max(1, 2, 3)", Some("3")),
            ("max(1,2,3)", None),
            ("max(1 ,2)", None),
            ("max(1;2)", Some("2")),
            ("max(1.5; 2)", Some("2")),
            ("min(1.5, 2)", Some("1.5")),
            ("round(2.456, 2)", Some("2.46")),
            ("1.5e3", Some("1500")),
            ("1,5e3", Some("1500")),
            ("1.5E-3", Some("0.0015")),
            ("2pi", Some("6.28318530717959")),
            ("pi", Some("3.14159265358979")),
            ("sin(30°)", Some("0.5")),
            ("sqrt(2)", Some("1.4142135623731")),
            ("255 in hex", Some("0xFF")),
            ("1,000 in hex", Some("0x3E8")),
            ("170!", Some("≈ 7.25741561530799e306")),
            ("171!", None),
            ("2x3", Some("6")),
            ("2xx3", None),
            ("0x1F + 1", Some("32")),
            ("3 !", Some("6")),
        ];
        for (t, want) in en_new {
            assert_eq!(shown(t, EN).as_deref(), *want, "en {t:?}");
        }
        assert!(it.len() + en.len() + it_new.len() + en_new.len() >= 150);
    }

    #[test]
    fn factorial_170_and_171() {
        let a = eval("170!", EN).unwrap();
        assert_eq!(a.shown, "≈ 7.25741561530799e306");
        assert_eq!(a.raw, "7.25741561530799e306");
        assert!(eval("171!", EN).is_none());
        assert!(eval("171!", IT).is_none());
        assert!(eval("170,5!", IT).is_none());
        assert!(eval("-1!", EN).unwrap().shown == "-1");
        assert!(eval("(0-1)!", EN).is_none());
        assert!(eval("171!/171!", EN).is_none(), "an infinite middle value is not an answer");
        assert_eq!(eval("170!/169!", EN).unwrap().shown, "170");
    }

    #[test]
    fn hex_round_trip() {
        for (text, shown) in [
            ("255 in hex", "0xFF"),
            ("0 in hex", "0x0"),
            ("4095 in hex", "0xFFF"),
            ("255 in bin", "0b11111111"),
            ("8 in bin", "0b1000"),
            ("255 in oct", "0o377"),
            ("-8 in oct", "-0o10"),
        ] {
            let a = eval(text, EN).unwrap();
            assert_eq!((a.shown.as_str(), a.raw.as_str()), (shown, shown), "{text}");
            // What the answer copies is a literal that reads back to the same number, in either locale.
            for n in [EN, IT] {
                let back = eval(&a.raw, n).unwrap();
                let want = eval(text.split(" in ").next().unwrap(), n).unwrap();
                assert_eq!(back.raw, want.raw, "{text} in {n:?}");
            }
        }
        // The decimal answer keeps 15 digits, so the check stays below 1e15.
        for v in [1u64, 15, 16, 255, 256, 65_535, 1 << 40, 1 << 49] {
            for base in ["hex", "bin", "oct"] {
                let a = eval(&format!("{v} in {base}"), EN).unwrap();
                let back = eval(&a.raw, EN).unwrap();
                assert_eq!(back.raw, v.to_string(), "{v} in {base}");
            }
        }
    }

    #[test]
    fn normalised_shows_what_was_read() {
        let n = |t: &str, s: NumberStyle| eval(t, s).unwrap().normalised;
        assert_eq!(n("2pi", EN), "2 × π");
        assert_eq!(n("2(3)", EN), "2 × (3)");
        assert_eq!(n("2 sqrt(16)", EN), "2 × sqrt(16)");
        assert_eq!(n("max(1, 2)", EN), "max(1; 2)");
        assert_eq!(n("max(1,5;2)", IT), "max(1,5; 2)");
        assert_eq!(n("3!+30°", EN), "3! + 30°");
        assert_eq!(n("255 in hex", EN), "255 in hex");
        assert_eq!(n("0xFF+1", EN), "255 + 1");
        assert_eq!(n("1e3", EN), "1000");
        assert_eq!(n("2x3", EN), "2 × 3");
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
        // A lone constant, a literal or a name is not a sum; a function, a factorial or a base answers by itself.
        for t in ["pi", "e", "π", "0xFF", "sqrt", "min", "e2", "in hex"] {
            assert!(!is_implicit(t, EN), "{t}");
        }
        for t in ["2pi", "2e", "(pi)", "sqrt(4)", "3!", "30°", "255 in hex", "min(1;2)", "2(3)", "pi*2", "1e3+1"] {
            assert!(is_implicit(t, EN), "{t}");
        }
    }

    #[test]
    fn seeded_100k_never_panics() {
        let mut r = XorShift::new(0xCA1C);
        let styles = [IT, EN];
        let alphabet = [
            '0', '1', '2', '9', '.', ',', '+', '-', '*', '/', '^', '%', '(', ')', ' ', 'x', '×', '÷', '!', '°', ';', 'e', 'p', 'i',
            'b', 'o', 'F',
        ];
        let glue = ["1", "2", "(", ")", ";", "!", "+"];
        let words = ["pi", "e", "sqrt(", "min(", "max(", "log2(", "sin(", "0x", "0b", "0o", " in ", "hex", "bin", "1e", "1e5"];
        for i in 0..100_000 {
            let n = styles[i % 2];
            let t = if i % 3 == 0 {
                r.ascii_or_bmp(40)
            } else if i % 3 == 1 {
                let len = r.next_u64() % 12;
                let mut t = String::new();
                for _ in 0..len {
                    let piece = if r.next_u64().is_multiple_of(3) { *r.pick(&words) } else { *r.pick(&glue) };
                    t.push_str(piece);
                }
                t
            } else {
                let len = r.next_u64() % 40;
                (0..len).map(|_| *r.pick(&alphabet)).collect()
            };
            let _ = eval(&t, n);
            let _ = is_implicit(&t, n);
        }
    }
}
