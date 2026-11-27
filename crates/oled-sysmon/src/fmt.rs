//! Number formats in a 16-byte cell.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumFmt {
    pub decimal: u8,
    pub group: u8,
}

impl NumFmt {
    pub const EN: NumFmt = NumFmt { decimal: b'.', group: b',' };
    pub const IT: NumFmt = NumFmt { decimal: b',', group: b'.' };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Unit {
    #[default]
    None,
    B,
    KB,
    MB,
    GB,
    TB,
    BPerS,
    KBPerS,
    MBPerS,
    GBPerS,
    TBPerS,
    Bps,
    Kbps,
    Mbps,
    Gbps,
}

impl Unit {
    /// "", "B", "KB", "MB", "GB", "TB", "B/s", "KB/s", "MB/s", "GB/s", "TB/s", "bps", "Kbps", "Mbps", "Gbps".
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::None => "",
            Unit::B => "B",
            Unit::KB => "KB",
            Unit::MB => "MB",
            Unit::GB => "GB",
            Unit::TB => "TB",
            Unit::BPerS => "B/s",
            Unit::KBPerS => "KB/s",
            Unit::MBPerS => "MB/s",
            Unit::GBPerS => "GB/s",
            Unit::TBPerS => "TB/s",
            Unit::Bps => "bps",
            Unit::Kbps => "Kbps",
            Unit::Mbps => "Mbps",
            Unit::Gbps => "Gbps",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tone {
    #[default]
    Normal,
    Tertiary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Cell {
    text: [u8; 12],
    len: u8,
    unit: Unit,
    tone: Tone,
    _pad: u8,
}

const DASH: &[u8] = "\u{2014}".as_bytes();

impl Cell {
    pub const EMPTY: Cell = Cell { text: [0; 12], len: 0, unit: Unit::None, tone: Tone::Normal, _pad: 0 };
    /// "—", Tertiary.
    pub fn unavailable() -> Cell {
        let mut c = Cell::EMPTY;
        c.text[..DASH.len()].copy_from_slice(DASH);
        c.len = DASH.len() as u8;
        c.tone = Tone::Tertiary;
        c
    }
    pub fn text(&self) -> &str {
        let n = (self.len as usize).min(self.text.len());
        std::str::from_utf8(&self.text[..n]).unwrap_or("")
    }
    pub fn unit(&self) -> Unit {
        self.unit
    }
    pub fn tone(&self) -> Tone {
        self.tone
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// FNV-1a over the 16 bytes; the UI folds heat and state into its own signature.
    pub fn signature(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let tail = [self.len, self.unit as u8, self.tone as u8, self._pad];
        for b in self.text.iter().chain(tail.iter()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    fn put(&mut self, b: u8) {
        if (self.len as usize) < self.text.len() {
            self.text[self.len as usize] = b;
            self.len += 1;
        }
    }
    /// Plain digits of `v`, no grouping.
    fn put_digits(&mut self, mut v: u64) {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for b in tmp[i..].iter() {
            self.put(*b);
        }
    }
    /// `scaled` holds the value times 10^decimals.
    fn put_fixed(&mut self, scaled: u64, decimals: u32, nf: NumFmt) {
        let div = 10u64.pow(decimals);
        self.put_digits(scaled / div);
        if decimals > 0 {
            self.put(nf.decimal);
            let frac = scaled % div;
            let mut d = div / 10;
            while d > 0 {
                self.put(b'0' + ((frac / d) % 10) as u8);
                d /= 10;
            }
        }
    }
}

/// Past this the digits do not fit a cell: the percent and the count draw the dash.
const MAX_COUNT: u64 = 999_999_999;

/// One decimal; 0 is empty, a nonzero value that rounds to 0.0 is "0.0" in the tertiary tone, 100 and
/// above have no decimals. NaN, infinity and negative values are the dash.
pub fn percent(v: f32, nf: NumFmt, out: &mut Cell) {
    *out = Cell::EMPTY;
    if !v.is_finite() || v < 0.0 || v as f64 > MAX_COUNT as f64 {
        *out = Cell::unavailable();
        return;
    }
    if v == 0.0 {
        return;
    }
    let tenths = (v as f64 * 10.0).round() as u64;
    if tenths >= 1000 {
        out.put_digits((v as f64).round() as u64);
    } else {
        out.put_fixed(tenths, 1, nf);
        if tenths == 0 {
            out.tone = Tone::Tertiary;
        }
    }
}

/// Three significant digits of `x`: 2 decimals below 10, 1 below 100, none from 100. A value that
/// rounds up to the next power of ten loses one decimal ("100", never "100.0").
fn put_three(out: &mut Cell, x: f64, nf: NumFmt) {
    let mut dec: u32 = if x >= 100.0 {
        0
    } else if x >= 10.0 {
        1
    } else {
        2
    };
    let mut scaled = (x * 10f64.powi(dec as i32)).round() as u64;
    while dec > 0 && scaled >= 1000 {
        dec -= 1;
        scaled = (x * 10f64.powi(dec as i32)).round() as u64;
    }
    out.put_fixed(scaled, dec, nf);
}

fn scaled_cell(v: u64, base: f64, units: &[Unit], nf: NumFmt, out: &mut Cell) {
    *out = Cell::EMPTY;
    let mut x = v as f64;
    let mut u = 0;
    while x >= 999.5 && u < units.len() - 1 {
        x /= base;
        u += 1;
    }
    if u == 0 {
        out.put_digits(v);
    } else {
        put_three(out, x, nf);
    }
    out.unit = units[u];
}

/// Binary units, three significant digits ("86.4" MB, "612" MB, "3.10" GB).
pub fn bytes(v: u64, nf: NumFmt, out: &mut Cell) {
    scaled_cell(v, 1024.0, &[Unit::B, Unit::KB, Unit::MB, Unit::GB, Unit::TB], nf, out);
}

/// Bytes per second; zero draws nothing.
pub fn rate(v: u64, nf: NumFmt, out: &mut Cell) {
    scaled_cell(v, 1024.0, &[Unit::BPerS, Unit::KBPerS, Unit::MBPerS, Unit::GBPerS, Unit::TBPerS], nf, out);
    if v == 0 {
        *out = Cell::EMPTY;
    }
}

/// Bits per second in SI steps; zero draws nothing.
pub fn bits(v: u64, nf: NumFmt, out: &mut Cell) {
    scaled_cell(v, 1000.0, &[Unit::Bps, Unit::Kbps, Unit::Mbps, Unit::Gbps], nf, out);
    if v == 0 {
        *out = Cell::EMPTY;
    }
}

/// A whole number with group separators, up to 999,999,999; more is the dash.
pub fn count(v: u64, nf: NumFmt, out: &mut Cell) {
    *out = Cell::EMPTY;
    if v > MAX_COUNT {
        *out = Cell::unavailable();
        return;
    }
    let mut digits = Cell::EMPTY;
    digits.put_digits(v);
    let n = digits.len as usize;
    for (i, b) in digits.text[..n].iter().enumerate() {
        let rest = (n - i) % 3;
        if i > 0 && rest == 0 {
            out.put(nf.group);
        }
        out.put(*b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(f: impl Fn(&mut Cell)) -> (String, Unit, Tone) {
        let mut c = Cell::default();
        f(&mut c);
        (c.text().to_string(), c.unit(), c.tone())
    }
    fn s(t: &str, u: Unit) -> (String, Unit, Tone) {
        (t.to_string(), u, Tone::Normal)
    }

    #[test]
    fn cell_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Cell>(), 16);
        assert_eq!(Unit::Gbps.as_str(), "Gbps");
        assert_eq!(Unit::TBPerS.as_str(), "TB/s");
        assert_eq!(Unit::None.as_str(), "");
    }

    #[test]
    fn percent_rules() {
        let en = NumFmt::EN;
        assert!(cell(|c| percent(0.0, en, c)).0.is_empty());
        assert_eq!(cell(|c| percent(0.04, en, c)), ("0.0".into(), Unit::None, Tone::Tertiary));
        assert_eq!(cell(|c| percent(0.4, en, c)), s("0.4", Unit::None));
        assert_eq!(cell(|c| percent(12.0, en, c)), s("12.0", Unit::None));
        assert_eq!(cell(|c| percent(99.96, en, c)), s("100", Unit::None));
        assert_eq!(cell(|c| percent(100.0, en, c)), s("100", Unit::None));
        assert_eq!(cell(|c| percent(f32::NAN, en, c)), ("\u{2014}".into(), Unit::None, Tone::Tertiary));
        assert_eq!(cell(|c| percent(12.5, NumFmt::IT, c)), s("12,5", Unit::None));
        assert_eq!(cell(|c| percent(400.0, en, c)), s("400", Unit::None));
        assert_eq!(cell(|c| percent(-1.0, en, c)).0, "\u{2014}");
    }

    #[test]
    fn bytes_three_digits() {
        let en = NumFmt::EN;
        assert_eq!(cell(|c| bytes(412, en, c)), s("412", Unit::B));
        assert_eq!(cell(|c| bytes(999, en, c)), s("999", Unit::B));
        assert_eq!(cell(|c| bytes(1000, en, c)), s("0.98", Unit::KB));
        assert_eq!(cell(|c| bytes(90_546_995, en, c)), s("86.4", Unit::MB));
        assert_eq!(cell(|c| bytes(641_728_512, en, c)), s("612", Unit::MB));
        assert_eq!(cell(|c| bytes(3_328_599_654, en, c)), s("3.10", Unit::GB));
        assert_eq!(cell(|c| bytes(3_328_599_654, NumFmt::IT, c)), s("3,10", Unit::GB));
        // 99.96 MB must not become "100.0".
        let edge = (99.96 * 1_048_576.0) as u64;
        assert_eq!(cell(|c| bytes(edge, en, c)), s("100", Unit::MB));
        // 9.996 MB rounds to "10.0", three digits.
        let edge = (9.996 * 1_048_576.0) as u64;
        assert_eq!(cell(|c| bytes(edge, en, c)), s("10.0", Unit::MB));
        // 1023.9 KB carries into the next unit.
        assert_eq!(cell(|c| bytes(1_048_000, en, c)), s("1.00", Unit::MB));
        assert_eq!(cell(|c| bytes(0, en, c)), s("0", Unit::B));
        assert!(cell(|c| bytes(u64::MAX, en, c)).0.len() <= 12);
    }

    #[test]
    fn rate_zero_draws_nothing() {
        let en = NumFmt::EN;
        let (t, u, _) = cell(|c| rate(0, en, c));
        assert!(t.is_empty() && u == Unit::None);
        assert_eq!(cell(|c| rate(512, en, c)), s("512", Unit::BPerS));
        assert_eq!(cell(|c| rate(5 * 1_048_576, en, c)), s("5.00", Unit::MBPerS));
        assert_eq!(cell(|c| rate(3 << 30, en, c)), s("3.00", Unit::GBPerS));
    }

    #[test]
    fn bits_si() {
        let en = NumFmt::EN;
        assert_eq!(cell(|c| bits(840_000, en, c)), s("840", Unit::Kbps));
        assert_eq!(cell(|c| bits(12_400_000, en, c)), s("12.4", Unit::Mbps));
        assert_eq!(cell(|c| bits(500, en, c)), s("500", Unit::Bps));
        assert_eq!(cell(|c| bits(2_500_000_000, en, c)), s("2.50", Unit::Gbps));
        assert!(cell(|c| bits(0, en, c)).0.is_empty());
        assert_eq!(cell(|c| bits(u64::MAX, en, c)).1, Unit::Gbps);
    }

    #[test]
    fn count_grouping() {
        assert_eq!(cell(|c| count(4211, NumFmt::EN, c)).0, "4,211");
        assert_eq!(cell(|c| count(4211, NumFmt::IT, c)).0, "4.211");
        assert_eq!(cell(|c| count(98_120, NumFmt::EN, c)).0, "98,120");
        assert_eq!(cell(|c| count(999_999_999, NumFmt::EN, c)).0, "999,999,999");
        assert_eq!(cell(|c| count(0, NumFmt::EN, c)).0, "0");
        assert_eq!(cell(|c| count(100, NumFmt::EN, c)).0, "100");
        assert_eq!(cell(|c| count(1_000_000_000_000, NumFmt::EN, c)), ("\u{2014}".into(), Unit::None, Tone::Tertiary));
    }

    #[test]
    fn signature_differs_per_content() {
        let mut a = Cell::default();
        let mut b = Cell::default();
        bytes(412, NumFmt::EN, &mut a);
        bytes(413, NumFmt::EN, &mut b);
        assert_ne!(a.signature(), b.signature());
        let mut c = Cell::default();
        rate(412, NumFmt::EN, &mut c);
        assert_ne!(a.signature(), c.signature());
        let mut a2 = Cell::default();
        bytes(412, NumFmt::EN, &mut a2);
        assert_eq!(a.signature(), a2.signature());
        assert_ne!(Cell::EMPTY.signature(), Cell::unavailable().signature());
    }
}
