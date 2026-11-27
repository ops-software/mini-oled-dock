//! The keyed-strings codemod of the dock (plan P1-workspace-v2, W4a). Std only.
//!
//! `extract` inventories the `tr("en", "it")` sites of src/, `apply` rewrites them to `t(k::KEY)` and fills
//! strings/en.toml and strings/it.toml, `tidy` and `prune` repair the toml files after a rebase and `verify` proves
//! that no text changed. Every command works on a root folder holding src/ and strings/.

pub mod lexer;
pub mod ops;
pub mod toml;

#[cfg(test)]
mod tests;
