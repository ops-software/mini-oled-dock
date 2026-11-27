//! oled-lang --lang it --version 1.8.0 [--root <workspace>] --out <file>
//!
//! Writes the it.lang of a workspace: every `strings` folder under the root (default: the current folder).

use std::path::PathBuf;
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let (mut lang, mut version, mut root, mut out) = (None, None, PathBuf::from("."), None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} wants a value"));
        match a.as_str() {
            "--lang" => lang = Some(value()?),
            "--version" => version = Some(value()?),
            "--root" => root = PathBuf::from(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let lang = lang.ok_or("--lang is required")?;
    let version = oled_i18n_build::lang::parse_version(&version.ok_or("--version is required")?)?;
    let out = out.ok_or("--out is required")?;
    let dirs = oled_i18n_build::lang::find_dirs(&root)?;
    if dirs.is_empty() {
        return Err(format!("no strings folder under {}", root.display()));
    }
    let bytes = oled_i18n_build::lang::build(&dirs, &lang, version)?;
    std::fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("{}: {} bytes from {} folders", out.display(), bytes.len(), dirs.len());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oled-lang: {e}");
            ExitCode::from(2)
        }
    }
}
