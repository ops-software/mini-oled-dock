//! The launcher (SPEC-launcher): pure model, parsers and ranking. Nothing in here calls Windows (lint 3 in `lints.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

#[cfg(test)]
mod lints;
#[cfg(test)]
mod testkit;
