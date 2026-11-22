//! Unit conversion (5.4.4, L2-U1): "<number>[ ]<unit> (in|to|a|->) <unit>", pure code. Families: length, mass,
//! temperature (affine, through Celsius), volume, area, speed, time, data and angle, with symbols and English and
//! Italian names in one static table. Numbers are read and results written by the calculator's rules (the locale's
//! marks, 15 significant digits). No currency, no calendar units (a month or a year has no fixed length).

use super::calc;
use super::model::NumberStyle;

/// The longest text read (the calculator's limit).
const MAX_CHARS: usize = 256;
/// The most words after the number: two units of up to three words and a connector fit.
const MAX_WORDS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Length,
    Mass,
    Temperature,
    Volume,
    Area,
    Speed,
    Time,
    Data,
    Angle,
}

/// One unit: its family, its size in the family's base unit (metre, kilogram, litre, square metre, metre per second,
/// second, byte, radian; unused for temperature), the symbol shown and every name it answers to.
struct Unit {
    family: Family,
    size: f64,
    symbol: &'static str,
    names: &'static [&'static str],
}

const fn unit(family: Family, size: f64, symbol: &'static str, names: &'static [&'static str]) -> Unit {
    Unit { family, size, symbol, names }
}

use Family::{Angle, Area, Data, Length, Mass, Speed, Temperature, Time, Volume};

/// Case-sensitive names are tried first ("Mb" is a megabit, "MB" a megabyte), then the same names ignoring ASCII case
/// ("mb" is a megabyte: the first unit of the table that matches, so bytes come before bits).
static UNITS: &[Unit] = &[
    unit(Length, 1.0, "m", &["m", "meter", "meters", "metre", "metres", "metro", "metri"]),
    unit(
        Length,
        1000.0,
        "km",
        &["km", "kilometer", "kilometers", "kilometre", "kilometres", "chilometro", "chilometri"],
    ),
    unit(
        Length,
        0.01,
        "cm",
        &["cm", "centimeter", "centimeters", "centimetre", "centimetres", "centimetro", "centimetri"],
    ),
    unit(
        Length,
        0.001,
        "mm",
        &["mm", "millimeter", "millimeters", "millimetre", "millimetres", "millimetro", "millimetri"],
    ),
    unit(Length, 1609.344, "mi", &["mi", "mile", "miles", "miglio", "miglia"]),
    unit(Length, 0.9144, "yd", &["yd", "yard", "yards", "iarda", "iarde"]),
    unit(Length, 0.3048, "ft", &["ft", "foot", "feet", "piede", "piedi"]),
    unit(Length, 0.0254, "in", &["in", "inch", "inches", "pollice", "pollici"]),
    unit(
        Mass,
        1.0,
        "kg",
        &["kg", "kilogram", "kilograms", "kilo", "kilos", "chilo", "chili", "chilogrammo", "chilogrammi"],
    ),
    unit(Mass, 0.001, "g", &["g", "gram", "grams", "grammo", "grammi"]),
    unit(Mass, 0.000_001, "mg", &["mg", "milligram", "milligrams", "milligrammo", "milligrammi"]),
    unit(Mass, 0.453_592_37, "lb", &["lb", "lbs", "pound", "pounds", "libbra", "libbre"]),
    unit(Mass, 0.028_349_523_125, "oz", &["oz", "ounce", "ounces", "oncia", "once"]),
    unit(Mass, 1000.0, "t", &["t", "tonne", "tonnes", "tonnellata", "tonnellate"]),
    unit(
        Temperature,
        0.0,
        "°C",
        &["c", "°c", "celsius", "centigrade", "deg c", "degrees celsius", "gradi celsius", "grado celsius"],
    ),
    unit(
        Temperature,
        0.0,
        "°F",
        &["f", "°f", "fahrenheit", "deg f", "degrees fahrenheit", "gradi fahrenheit", "grado fahrenheit"],
    ),
    unit(Temperature, 0.0, "K", &["k", "kelvin"]),
    unit(Volume, 1.0, "l", &["l", "liter", "liters", "litre", "litres", "litro", "litri"]),
    unit(
        Volume,
        0.001,
        "ml",
        &["ml", "milliliter", "milliliters", "millilitre", "millilitres", "millilitro", "millilitri"],
    ),
    unit(
        Volume,
        0.01,
        "cl",
        &["cl", "centiliter", "centiliters", "centilitre", "centilitres", "centilitro", "centilitri"],
    ),
    unit(Volume, 3.785_411_784, "gal", &["gal", "gallon", "gallons", "galone", "galloni"]),
    unit(Volume, 0.946_352_946, "qt", &["qt", "quart", "quarts"]),
    unit(Volume, 0.473_176_473, "pt", &["pt", "pint", "pints", "pinta", "pinte"]),
    unit(Volume, 0.029_573_529_562_5, "fl oz", &["fl oz", "floz", "fluid ounce", "fluid ounces"]),
    unit(Volume, 0.236_588_236_5, "cup", &["cup", "cups", "tazza", "tazze"]),
    unit(Volume, 0.014_786_764_781_25, "tbsp", &["tbsp", "tablespoon", "tablespoons", "cucchiaio", "cucchiai"]),
    unit(Volume, 0.004_928_921_593_75, "tsp", &["tsp", "teaspoon", "teaspoons", "cucchiaino", "cucchiaini"]),
    unit(Volume, 1000.0, "m³", &["m3", "m³", "cubic meter", "cubic meters", "metro cubo", "metri cubi"]),
    unit(
        Area,
        1.0,
        "m²",
        &["m2", "m²", "sqm", "mq", "square meter", "square meters", "metro quadrato", "metri quadrati"],
    ),
    unit(
        Area,
        1_000_000.0,
        "km²",
        &["km2", "km²", "kmq", "square kilometer", "square kilometers", "chilometro quadrato", "chilometri quadrati"],
    ),
    unit(Area, 0.0001, "cm²", &["cm2", "cm²", "cmq"]),
    unit(Area, 10_000.0, "ha", &["ha", "hectare", "hectares", "ettaro", "ettari"]),
    unit(Area, 4_046.856_422_4, "acre", &["acre", "acres", "acro", "acri"]),
    unit(
        Area,
        0.092_903_04,
        "ft²",
        &["ft2", "ft²", "sqft", "square foot", "square feet", "piede quadrato", "piedi quadrati"],
    ),
    unit(Speed, 1.0, "m/s", &["m/s", "mps"]),
    unit(Speed, 1000.0 / 3600.0, "km/h", &["km/h", "kph", "kmh", "kmph"]),
    unit(Speed, 0.447_04, "mph", &["mph", "mi/h"]),
    unit(Speed, 1852.0 / 3600.0, "kn", &["kn", "kt", "knot", "knots", "nodo", "nodi"]),
    unit(Speed, 0.3048, "ft/s", &["ft/s", "fps"]),
    unit(Time, 1.0, "s", &["s", "sec", "secs", "second", "seconds", "secondo", "secondi"]),
    unit(Time, 0.001, "ms", &["ms", "millisecond", "milliseconds", "millisecondo", "millisecondi"]),
    unit(Time, 60.0, "min", &["min", "mins", "minute", "minutes", "minuto", "minuti"]),
    unit(Time, 3600.0, "h", &["h", "hr", "hrs", "hour", "hours", "ora", "ore"]),
    unit(Time, 86_400.0, "d", &["d", "day", "days", "giorno", "giorni"]),
    unit(Time, 604_800.0, "wk", &["wk", "week", "weeks", "settimana", "settimane"]),
    unit(Data, 1.0, "B", &["B", "byte", "bytes"]),
    unit(Data, 1e3, "kB", &["kB", "kilobyte", "kilobytes"]),
    unit(Data, 1e6, "MB", &["MB", "megabyte", "megabytes"]),
    unit(Data, 1e9, "GB", &["GB", "gigabyte", "gigabytes"]),
    unit(Data, 1e12, "TB", &["TB", "terabyte", "terabytes"]),
    unit(Data, 1e15, "PB", &["PB", "petabyte", "petabytes"]),
    unit(Data, 1024.0, "KiB", &["KiB", "kibibyte", "kibibytes"]),
    unit(Data, 1_048_576.0, "MiB", &["MiB", "mebibyte", "mebibytes"]),
    unit(Data, 1_073_741_824.0, "GiB", &["GiB", "gibibyte", "gibibytes"]),
    unit(Data, 1_099_511_627_776.0, "TiB", &["TiB", "tebibyte", "tebibytes"]),
    unit(Data, 0.125, "bit", &["b", "bit", "bits"]),
    unit(Data, 125.0, "kbit", &["kbit", "Kb", "kilobit", "kilobits"]),
    unit(Data, 125_000.0, "Mbit", &["Mbit", "Mb", "megabit", "megabits"]),
    unit(Data, 125_000_000.0, "Gbit", &["Gbit", "Gb", "gigabit", "gigabits"]),
    unit(Angle, 1.0, "rad", &["rad", "radian", "radians", "radiante", "radianti"]),
    unit(Angle, std::f64::consts::PI / 180.0, "°", &["°", "deg", "degree", "degrees", "grado", "gradi"]),
];

/// The words that join the two units.
fn is_connector(w: &str) -> bool {
    ["in", "to", "a", "->", "→"].iter().any(|c| w.eq_ignore_ascii_case(c))
}

fn find(name: &str) -> Option<&'static Unit> {
    UNITS
        .iter()
        .find(|u| u.names.contains(&name))
        .or_else(|| UNITS.iter().find(|u| u.names.iter().any(|n| n.eq_ignore_ascii_case(name))))
}

/// The unit named by these words (joined by a space: "fluid ounces", "gradi celsius").
fn unit_of(words: &[&str]) -> Option<&'static Unit> {
    match words {
        [] => None,
        [one] => find(one),
        many => find(&many.join(" ")),
    }
}

/// A temperature in Celsius, and back; the scales are affine, not proportional.
fn to_celsius(u: &Unit, v: f64) -> f64 {
    match u.symbol {
        "°F" => (v - 32.0) * 5.0 / 9.0,
        "K" => v - 273.15,
        _ => v,
    }
}

fn from_celsius(u: &Unit, c: f64) -> f64 {
    match u.symbol {
        "°F" => c * 9.0 / 5.0 + 32.0,
        "K" => c + 273.15,
        _ => c,
    }
}

/// What the answer row shows: the value and the target symbol, the number alone (what Enter copies) and the question as
/// the reader understood it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Conversion {
    pub shown: String,
    pub raw: String,
    pub detail: String,
}

/// Converts "<number> <unit> in <unit>" (the connector is the last of in, to, a, -> that leaves a unit on both sides,
/// so "10 in in cm" is inches). None for anything else, and at once, without allocating, for a text whose first
/// character is not a digit, a sign or the decimal mark.
pub fn convert(text: &str, num: NumberStyle) -> Option<Conversion> {
    let text = text.trim();
    let first = text.chars().next()?;
    if !(first.is_ascii_digit() || matches!(first, '+' | '-' | '−') || first == num.decimal) {
        return None;
    }
    if text.chars().count() > MAX_CHARS {
        return None;
    }
    let (negative, body) = match first {
        '-' | '−' => (true, text.get(first.len_utf8()..)?),
        '+' => (false, text.get(1..)?),
        _ => (false, text),
    };
    let end = body.find(|c: char| !(c.is_ascii_digit() || c == num.decimal || c == num.group)).unwrap_or(body.len());
    let run = body.get(..end)?;
    if !run.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let (value, _) = calc::number(run, num)?;
    let value = if negative { -value } else { value };
    let mut words = [""; MAX_WORDS];
    let mut n = 0;
    for w in body.get(end..)?.split_whitespace() {
        *words.get_mut(n)? = w;
        n += 1;
    }
    let words = words.get(..n)?;
    let (from, to) = (1..n.saturating_sub(1)).rev().find_map(|k| {
        let w = *words.get(k)?;
        if !is_connector(w) {
            return None;
        }
        let from = unit_of(words.get(..k)?)?;
        let to = unit_of(words.get(k + 1..)?)?;
        Some((from, to))
    })?;
    if from.family != to.family || !value.is_finite() {
        return None;
    }
    let out = if from.family == Temperature {
        let out = from_celsius(to, to_celsius(from, value));
        // Cancellation leaves noise around zero ("32 F in C").
        if out.abs() < 1e-9 {
            0.0
        } else {
            out
        }
    } else {
        value * from.size / to.size
    };
    if !out.is_finite() {
        return None;
    }
    let raw = calc::format_plain(out, num);
    Some(Conversion {
        shown: format!("{raw} {}", to.symbol),
        detail: format!("{} {} → {}", calc::format_plain(value, num), from.symbol, to.symbol),
        raw,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::{allocations_during, XorShift};

    const EN: NumberStyle = NumberStyle { decimal: '.', group: ',' };
    const IT: NumberStyle = NumberStyle { decimal: ',', group: '.' };

    /// (text, marks, the row's title, what Enter copies): 107 conversions, every family, both languages, both
    /// decimal marks, attached and spaced units, the ambiguities ("in", "b" and "B", "mb" and "Mb").
    const VECTORS: &[(&str, NumberStyle, &str, &str)] = &[
        ("1 km to m", EN, "1000 m", "1000"),
        ("10 km in mi", EN, "6.21371192237334 mi", "6.21371192237334"),
        ("1 mi to km", EN, "1.609344 km", "1.609344"),
        ("100 cm in m", EN, "1 m", "1"),
        ("12 in to ft", EN, "1 ft", "1"),
        ("1 ft to in", EN, "12 in", "12"),
        ("3 ft to yd", EN, "1 yd", "1"),
        ("1 yd to m", EN, "0.9144 m", "0.9144"),
        ("5280 ft in mi", EN, "1 mi", "1"),
        ("1 mi in yd", EN, "1760 yd", "1760"),
        ("1000 mm to m", EN, "1 m", "1"),
        ("2,5 km a m", IT, "2500 m", "2500"),
        ("3 chilometri a metri", IT, "3000 m", "3000"),
        ("2 miglia in km", IT, "3,218688 km", "3,218688"),
        ("6 piedi in pollici", IT, "72 in", "72"),
        ("1 pollice a cm", IT, "2,54 cm", "2,54"),
        ("1 kg to g", EN, "1000 g", "1000"),
        ("1 lb to kg", EN, "0.45359237 kg", "0.45359237"),
        ("16 oz to lb", EN, "1 lb", "1"),
        ("1 oz to g", EN, "28.349523125 g", "28.349523125"),
        ("500 g to kg", EN, "0.5 kg", "0.5"),
        ("2 chili in libbre", IT, "4,40924524369755 lb", "4,40924524369755"),
        ("1 libbra in grammi", IT, "453,59237 g", "453,59237"),
        ("1000 mg to g", EN, "1 g", "1"),
        ("1 t to kg", EN, "1000 kg", "1000"),
        ("2 once a grammi", IT, "56,69904625 g", "56,69904625"),
        ("0 c to f", EN, "32 °F", "32"),
        ("100 c in f", EN, "212 °F", "212"),
        ("32 f to c", EN, "0 °C", "0"),
        ("212 f to c", EN, "100 °C", "100"),
        ("0 k to c", EN, "-273.15 °C", "-273.15"),
        ("-40 c in f", EN, "-40 °F", "-40"),
        ("300 k to c", EN, "26.85 °C", "26.85"),
        ("72 f in c", EN, "22.2222222222222 °C", "22.2222222222222"),
        ("37 °c in f", EN, "98.6 °F", "98.6"),
        ("20 gradi celsius in fahrenheit", IT, "68 °F", "68"),
        ("20 celsius to kelvin", EN, "293.15 K", "293.15"),
        ("1 l to ml", EN, "1000 ml", "1000"),
        ("1 gal to l", EN, "3.785411784 l", "3.785411784"),
        ("1 galone in litri", IT, "3,785411784 l", "3,785411784"),
        ("500 ml to l", EN, "0.5 l", "0.5"),
        ("1 qt to l", EN, "0.946352946 l", "0.946352946"),
        ("4 qt to gal", EN, "1 gal", "1"),
        ("1 tbsp to ml", EN, "14.78676478125 ml", "14.78676478125"),
        ("3 tsp to tbsp", EN, "1 tbsp", "1"),
        ("1 cup to ml", EN, "236.5882365 ml", "236.5882365"),
        ("2 pt to qt", EN, "1 qt", "1"),
        ("1 fl oz to ml", EN, "29.5735295625 ml", "29.5735295625"),
        ("1 m3 to l", EN, "1000 l", "1000"),
        ("1 ha to m2", EN, "10000 m²", "10000"),
        ("1 acre to m2", EN, "4046.8564224 m²", "4046.8564224"),
        ("1 km2 to ha", EN, "100 ha", "100"),
        ("1 acro in ettari", IT, "0,40468564224 ha", "0,40468564224"),
        ("10 ft2 to m2", EN, "0.9290304 m²", "0.9290304"),
        ("1 mq in cm2", IT, "10000 cm²", "10000"),
        ("36 km/h to m/s", EN, "10 m/s", "10"),
        ("60 mph to km/h", EN, "96.56064 km/h", "96.56064"),
        ("1 kn to km/h", EN, "1.852 km/h", "1.852"),
        ("100 km/h to m/s", EN, "27.7777777777778 m/s", "27.7777777777778"),
        ("10 m/s in km/h", EN, "36 km/h", "36"),
        ("1 nodo in km/h", IT, "1,852 km/h", "1,852"),
        ("1 h to min", EN, "60 min", "60"),
        ("1 d to h", EN, "24 h", "24"),
        ("90 min in h", EN, "1.5 h", "1.5"),
        ("1 wk to d", EN, "7 d", "7"),
        ("1 settimana in giorni", IT, "7 d", "7"),
        ("2 ore in minuti", IT, "120 min", "120"),
        ("1500 ms to s", EN, "1.5 s", "1.5"),
        ("1 day to s", EN, "86400 s", "86400"),
        ("3600 s to h", EN, "1 h", "1"),
        ("2 giorni in ore", IT, "48 h", "48"),
        ("1 kB to B", EN, "1000 B", "1000"),
        ("1 KiB to B", EN, "1024 B", "1024"),
        ("1,5 GiB to MB", IT, "1610,612736 MB", "1610,612736"),
        ("1.5 GiB to MB", EN, "1610.612736 MB", "1610.612736"),
        ("1 GB to MB", EN, "1000 MB", "1000"),
        ("1 GiB to MiB", EN, "1024 MiB", "1024"),
        ("8 b to B", EN, "1 B", "1"),
        ("1 B to b", EN, "8 bit", "8"),
        ("1 MB to Mb", EN, "8 Mbit", "8"),
        ("1 TB to GB", EN, "1000 GB", "1000"),
        ("100 Mb to MB", EN, "12.5 MB", "12.5"),
        ("1 kb to B", EN, "1000 B", "1000"),
        ("1 Gbit to MB", EN, "125 MB", "125"),
        ("180 deg to rad", EN, "3.14159265358979 rad", "3.14159265358979"),
        ("1 rad to deg", EN, "57.2957795130823 °", "57.2957795130823"),
        ("90 gradi in radianti", IT, "1,5707963267949 rad", "1,5707963267949"),
        ("180 ° in rad", EN, "3.14159265358979 rad", "3.14159265358979"),
        ("360 deg in rad", EN, "6.28318530717959 rad", "6.28318530717959"),
        ("1.234 m in cm", IT, "123400 cm", "123400"),
        ("1,234 m in cm", EN, "123400 cm", "123400"),
        ("0,5 kg a g", IT, "500 g", "500"),
        ("1.5 kg to g", EN, "1500 g", "1500"),
        ("-5 c to f", EN, "23 °F", "23"),
        ("+10 km to m", EN, "10000 m", "10000"),
        (".5 km to m", EN, "500 m", "500"),
        ("10km to mi", EN, "6.21371192237334 mi", "6.21371192237334"),
        ("72f in c", EN, "22.2222222222222 °C", "22.2222222222222"),
        ("5°c in f", EN, "41 °F", "41"),
        ("10 km -> mi", EN, "6.21371192237334 mi", "6.21371192237334"),
        ("10 km → mi", EN, "6.21371192237334 mi", "6.21371192237334"),
        ("10 in in cm", EN, "25.4 cm", "25.4"),
        ("1 in to cm", EN, "2.54 cm", "2.54"),
        ("5 ft in in", EN, "60 in", "60"),
        ("5 in in ft", EN, "0.416666666666667 ft", "0.416666666666667"),
        ("2 in in in", EN, "2 in", "2"),
        ("10 km to km", EN, "10 km", "10"),
    ];

    #[test]
    fn vectors_80() {
        assert!(VECTORS.len() >= 80);
        let mut bad = Vec::new();
        for (text, num, shown, raw) in VECTORS {
            match convert(text, *num) {
                Some(c) if c.shown == *shown && c.raw == *raw => {}
                other => bad.push(format!("{text:?}: wanted {shown:?} / {raw:?}, got {other:?}")),
            }
        }
        assert!(bad.is_empty(), "{} failures:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn the_question_is_normalised() {
        let c = convert("10km TO mi", EN).unwrap();
        assert_eq!(c.detail, "10 km → mi");
        assert_eq!(convert("2,5 chili a libbre", IT).unwrap().detail, "2,5 kg → lb");
        assert_eq!(convert("-40 c in f", EN).unwrap().detail, "-40 °C → °F");
        // Case does not matter for names, but does for the data units that differ only by it.
        assert_eq!(convert("1 MB to kb", EN).unwrap().shown, "1000 kB");
        assert_eq!(convert("1 MB to Kb", EN).unwrap().shown, "8000 kbit");
        assert_eq!(convert("  5 KM   TO   M  ", EN).unwrap().shown, "5000 m");
    }

    #[test]
    fn refusals() {
        let none = [
            "",
            "-",
            "+",
            ".",
            "10",
            "10 km",
            "10 km to",
            "10 to km",
            "km to m",
            "abc",
            "1 km to kg",
            "1 foo to m",
            "1 m to bar",
            "1 km to m to mi",
            "1.2.3 km to m",
            "1e5 m to km",
            "1 degrees to c",
            "1 a b c d e f g h i j to km",
            "- km to m",
            "0x10 m to km",
            "1 month to d",
            "1 usd to eur",
        ];
        for t in none {
            assert!(convert(t, EN).is_none(), "{t:?}");
            assert!(convert(t, IT).is_none(), "{t:?} (it)");
        }
        let long = format!("1 {}m to km", "k".repeat(300));
        assert!(convert(&long, EN).is_none());
        let long_number = format!("{} m to km", "1".repeat(300));
        assert!(convert(&long_number, EN).is_none());
    }

    #[test]
    fn no_allocation_without_a_number() {
        for t in ["calc", "km to m", "", "x1 m to km", "(1+2) m in km", "Ω", "=1", "$ m to km", "  word word"] {
            let (r, n) = allocations_during(|| convert(t, EN));
            assert!(r.is_none(), "{t:?}");
            assert_eq!(n, 0, "{t:?}");
        }
    }

    #[test]
    fn seeded_100k_never_panics() {
        let mut r = XorShift::new(0x0715);
        let names: Vec<&str> = UNITS.iter().flat_map(|u| u.names.iter().copied()).collect();
        let joins = ["in", "to", "a", "->", "→", "in in", " ", "to to"];
        let mut answers = 0;
        for i in 0..100_000 {
            let text = match i % 4 {
                0 => r.ascii_or_bmp(40),
                1 => format!("{} {} {} {}", r.next_u64() % 100_000, r.pick(&names), r.pick(&joins), r.pick(&names)),
                2 => format!("{}{} {} {}", r.ascii_or_bmp(3), r.next_u64() % 1000, r.pick(&names), r.ascii_or_bmp(6)),
                _ => format!(
                    "{},{}{} {} {}",
                    r.next_u64() % 99,
                    r.next_u64() % 999,
                    r.pick(&names),
                    r.pick(&joins),
                    r.pick(&names)
                ),
            };
            for num in [EN, IT] {
                if let Some(c) = convert(&text, num) {
                    answers += 1;
                    assert!(!c.shown.is_empty() && !c.raw.is_empty() && c.shown.starts_with(&c.raw), "{text:?}");
                }
            }
        }
        assert!(answers > 1000, "the generator reaches the answers: {answers}");
    }
}
