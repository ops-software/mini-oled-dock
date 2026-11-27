//! The suite's configuration engine: JSONC helpers, errors, descriptors, the options store and the theme file (F-P1, F-P2).
// PF1 freeze: the types below are frozen; bodies arrive with W5a, W5b and TH1.

pub mod desc;
pub mod error;
pub mod jsonc;
mod options;
pub mod store;
pub mod theme;

pub use desc::*;
pub use error::*;
pub use store::*;
