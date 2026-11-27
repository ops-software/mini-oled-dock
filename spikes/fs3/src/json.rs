//! A small JSON writer for the probes' records (no serde: the spike's only dependencies are windows and
//! embed-manifest). Objects keep the order their fields were set in.

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// An empty object.
    pub fn obj() -> J {
        J::Obj(Vec::new())
    }

    /// Sets a field of an object (replacing a field of the same name); any other value is returned unchanged.
    pub fn set(mut self, key: &str, value: impl Into<J>) -> J {
        if let J::Obj(fields) = &mut self {
            let value = value.into();
            match fields.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => fields.push((key.to_string(), value)),
            }
        }
        self
    }

    /// The field `key` of an object.
    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as a bool, when it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The record's text: two-space indentation, arrays of scalars on one line, a final newline.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn scalar(&self) -> bool {
        !matches!(self, J::Arr(_) | J::Obj(_))
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => out.push_str(&n.to_string()),
            J::Str(s) => escape(s, out),
            J::Arr(items) if items.iter().all(J::scalar) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out, depth + 1);
                }
                out.push(']');
            }
            J::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    out.push_str(if i > 0 { ",\n" } else { "\n" });
                    indent(out, depth + 1);
                    v.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push(']');
            }
            J::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            J::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    out.push_str(if i > 0 { ",\n" } else { "\n" });
                    indent(out, depth + 1);
                    escape(k, out);
                    out.push_str(": ");
                    v.write(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl From<bool> for J {
    fn from(b: bool) -> J {
        J::Bool(b)
    }
}
impl From<i64> for J {
    fn from(n: i64) -> J {
        J::Int(n)
    }
}
impl From<i32> for J {
    fn from(n: i32) -> J {
        J::Int(n.into())
    }
}
impl From<u32> for J {
    fn from(n: u32) -> J {
        J::Int(n.into())
    }
}
impl From<u64> for J {
    fn from(n: u64) -> J {
        J::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}
impl From<usize> for J {
    fn from(n: usize) -> J {
        J::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}
impl From<&str> for J {
    fn from(s: &str) -> J {
        J::Str(s.to_string())
    }
}
impl From<String> for J {
    fn from(s: String) -> J {
        J::Str(s)
    }
}
impl From<Vec<J>> for J {
    fn from(v: Vec<J>) -> J {
        J::Arr(v)
    }
}
impl<T: Into<J>> From<Option<T>> for J {
    fn from(v: Option<T>) -> J {
        v.map_or(J::Null, Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped() {
        let j = J::from("a\"b\\c\nd\u{1}e");
        assert_eq!(j.render(), "\"a\\\"b\\\\c\\nd\\u0001e\"\n");
    }

    #[test]
    fn objects_keep_their_order_and_replace_a_field() {
        let j = J::obj().set("b", 1u32).set("a", true).set("b", "x").set("n", None::<u32>);
        assert_eq!(j.render(), "{\n  \"b\": \"x\",\n  \"a\": true,\n  \"n\": null\n}\n");
        assert_eq!(j.get("a").and_then(J::as_bool), Some(true));
    }

    #[test]
    fn arrays_of_scalars_stay_on_one_line() {
        let j = J::obj().set("v", vec![J::from(1u32), J::from(2u32)]).set("o", vec![J::obj().set("k", 0u32)]);
        assert_eq!(j.render(), "{\n  \"v\": [1, 2],\n  \"o\": [\n    {\n      \"k\": 0\n    }\n  ]\n}\n");
    }
}
