//! The launcher (SPEC-launcher): pure model, parsers and ranking. Nothing in here calls Windows (lint 3 in `lints.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

pub mod calc;
pub mod catalog;
pub mod commands;
pub mod field;
pub mod gesture;
pub mod history;
pub mod keywords;
pub mod model;
pub mod providers;
pub mod query;
pub mod rank;
pub mod runline;
pub mod session;
pub mod strings;
pub mod style;
pub mod verbs;
pub mod web;

#[cfg(test)]
mod lints;
#[cfg(test)]
mod testkit;
