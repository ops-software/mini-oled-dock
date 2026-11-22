//! The launcher (SPEC-launcher): pure model, parsers and ranking. Nothing in here calls Windows (lint 3 in `lints.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
// The session that uses these modules arrives in L1a-8 to L1a-15; until then only the tests reach them.
#![allow(dead_code)]

pub mod calc;
pub mod field;
pub mod keywords;
pub mod model;
pub mod query;
pub mod rank;
pub mod strings;
pub mod style;
pub mod verbs;

#[cfg(test)]
mod lints;
#[cfg(test)]
mod testkit;
