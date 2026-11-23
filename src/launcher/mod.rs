//! The launcher (SPEC-launcher): pure model, parsers and ranking. Nothing in here calls Windows (lint 3 in `lints.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

pub mod aliases;
pub mod arena;
pub mod calc;
pub mod catalog;
pub mod commands;
pub mod datetime;
pub mod field;
// The helper child (L2-F2) and the pump (L2-F3) are its callers.
#[allow(dead_code)]
pub mod files;
pub mod frecency;
pub mod gesture;
pub mod history;
pub mod index;
pub mod keywords;
pub mod model;
pub mod powertoys;
pub mod providers;
pub mod query;
pub mod rank;
pub mod runline;
pub mod session;
pub mod strings;
pub mod style;
pub mod units;
pub mod verbs;
pub mod web;

#[cfg(test)]
mod lints;
#[cfg(test)]
mod rank_table;
#[cfg(test)]
pub(crate) mod testkit;
