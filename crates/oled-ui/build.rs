//! Generates the kit's keyed string table (FZ-I18N): strings/en.toml and it.toml become OUT_DIR/strings.rs, read by
//! src/strings.rs. The generator prints its own rerun-if-changed lines and fails the build on a key in one file only or
//! an empty value.

use std::path::{Path, PathBuf};

fn main() {
    let Some(dir) = std::env::var_os("OUT_DIR") else {
        panic!("OUT_DIR is not set: run through cargo");
    };
    let out = PathBuf::from(dir).join("strings.rs");
    if let Err(e) = oled_i18n_build::generate(Path::new("strings"), &out) {
        panic!("kit strings: {e}");
    }
}
