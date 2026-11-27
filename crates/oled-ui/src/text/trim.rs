//! Trimming a run of text to a width (P2-kit 2.3); the trimming itself is filled by K1a.2.

/// How a layout too wide for its max width is shortened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Trim {
    /// Never shortened.
    None,
    /// "Long na…" (the dock's ellipsize).
    #[default]
    End,
    /// "very-long…ft-07.docx": keeps the last `tail` chars (files uses 10).
    Middle { tail: u8 },
    /// "…\folder\name": keeps the end (paths).
    Start,
}
