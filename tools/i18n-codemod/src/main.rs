//! Command line of the codemod: `i18n-codemod <extract|apply|tidy|prune|verify> [options]`, run from the repository root.

use std::path::PathBuf;
use std::process::ExitCode;

use i18n_codemod::ops;

const USAGE: &str =
    "usage: i18n-codemod <extract [--expect N] | apply | tidy | prune | verify --base REV [--exempt KEY_PREFIX]...> \
                     [--root DIR]";

fn run(args: &[String]) -> Result<(), String> {
    let cmd = args.first().ok_or(USAGE)?.as_str();
    let mut root = PathBuf::from(".");
    let mut expect = None;
    let mut base = None;
    let mut exempt: Vec<String> = ops::EXEMPT_KEY_PREFIXES.iter().map(|s| s.to_string()).collect();
    let mut i = 1;
    while i < args.len() {
        let value = args.get(i + 1).ok_or_else(|| format!("{} wants a value\n{USAGE}", args[i]))?;
        match args[i].as_str() {
            "--root" => root = PathBuf::from(value),
            "--expect" => expect = Some(value.parse::<usize>().map_err(|e| format!("--expect: {e}"))?),
            "--base" => base = Some(value.clone()),
            "--exempt" => exempt.push(value.clone()),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
        i += 2;
    }
    match cmd {
        "extract" => {
            let tsv = root.join("scratch").join("i18n-baseline.tsv");
            let r = ops::extract(&root, expect, Some(&tsv))?;
            for s in &r.skipped {
                println!("non-literal: {s}");
            }
            println!(
                "extract: {} literal sites, {} non-literal skipped, {} tokens in src/i18n.rs, independent count {}; wrote {}",
                r.rows.len(),
                r.skipped.len(),
                r.own,
                r.independent,
                tsv.display()
            );
            Ok(())
        }
        "apply" => {
            let r = ops::apply(&root)?;
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
            println!("apply: {} sites converted, {} new keys, {} reused", r.converted, r.new_keys, r.reused);
            Ok(())
        }
        "tidy" => {
            let n = ops::tidy(&root)?;
            println!("tidy: {n} keys");
            Ok(())
        }
        "prune" => {
            for k in ops::prune(&root)? {
                println!("pruned {k}");
            }
            Ok(())
        }
        "verify" => {
            let rev = base.ok_or("verify wants --base <rev>")?;
            let old = ops::GitOld { root: root.clone(), rev };
            match ops::verify(&root, &old, &exempt) {
                Ok(n) => {
                    println!("verify: {n} sites checked, 0 mismatches");
                    Ok(())
                }
                Err(list) => {
                    for m in list.iter().take(20) {
                        println!("{m}");
                    }
                    Err(format!("verify: {} mismatches", list.len()))
                }
            }
        }
        _ => Err(USAGE.to_string()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
