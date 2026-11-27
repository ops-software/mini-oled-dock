//! Config errors, as logged and as shown.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub problem: Problem,
    pub kept: Kept,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Parse(String),
    Type { expected: &'static str },
    Range { min: i64, max: i64, got: i64 },
    UnknownValue { allowed: &'static [&'static str] },
    TooLong { max: usize },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    LastGood,
    Default,
}
impl ConfigError {
    /// One English line for the log: "line 14, app_icon_size: 40 is above the maximum 32 (kept the last good value)".
    /// The line is left out when it is not known (0), the key when the error is about the whole file.
    pub fn log_line(&self) -> String {
        let mut s = String::new();
        if self.line > 0 {
            s.push_str(&format!("line {}, ", self.line));
        }
        if !self.path.is_empty() {
            s.push_str(&self.path);
            s.push_str(": ");
        }
        match &self.problem {
            Problem::Parse(m) => s.push_str(m),
            Problem::Type { expected } => s.push_str(&format!("expected {expected}")),
            Problem::Range { max, got, .. } if got > max => s.push_str(&format!("{got} is above the maximum {max}")),
            Problem::Range { min, got, .. } if got < min => s.push_str(&format!("{got} is below the minimum {min}")),
            Problem::Range { min, max, got } => s.push_str(&format!("{got} is outside {min}..{max}")),
            Problem::UnknownValue { allowed } => s.push_str(&format!("not one of {}", allowed.join(", "))),
            Problem::TooLong { max } => s.push_str(&format!("longer than {max} characters")),
        }
        s.push_str(match self.kept {
            Kept::LastGood => " (kept the last good value)",
            Kept::Default => " (kept the default)",
        });
        s
    }

    /// A whole-file error from serde_json: its position in the fields, its message without the position.
    pub(crate) fn from_serde(e: &serde_json::Error, kept: Kept) -> ConfigError {
        let full = e.to_string();
        let tail = format!(" at line {} column {}", e.line(), e.column());
        let message = match full.strip_suffix(&tail) {
            Some(m) if e.line() > 0 => m.to_string(),
            _ => full,
        };
        ConfigError {
            path: String::new(),
            line: u32::try_from(e.line()).unwrap_or(u32::MAX),
            col: u32::try_from(e.column()).unwrap_or(u32::MAX),
            problem: Problem::Parse(message),
            kept,
        }
    }

    /// The text serde_json shows for a whole-file error: the message, then " at line L column C" when the position is
    /// known (the line the dock has always logged).
    pub(crate) fn serde_text(&self) -> String {
        let m = match &self.problem {
            Problem::Parse(m) => m.clone(),
            _ => self.log_line(),
        };
        if self.line > 0 {
            format!("{m} at line {} column {}", self.line, self.col)
        } else {
            m
        }
    }
}
