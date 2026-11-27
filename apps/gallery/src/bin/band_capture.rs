//! band_capture.exe (P2-kit S-BAND, K2a.11; dev only, unelevated): starts `gallery --measure band` on a monitor and
//! counts torn frames through DXGI desktop duplication, reading only the gallery window's pixels.

/// `--monitor <i> --secs <n>`, both required.
fn parse(args: &[String]) -> Result<(u32, u32), String> {
    let (mut monitor, mut secs) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--monitor" => &mut monitor,
            "--secs" => &mut secs,
            other => return Err(format!("unknown argument {other:?}")),
        };
        if slot.is_some() {
            return Err(format!("{a} is given twice"));
        }
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        *slot = Some(v.parse::<u32>().map_err(|_| format!("{a} {v:?}: not a number"))?);
    }
    match (monitor, secs) {
        (Some(m), Some(s)) if s > 0 => Ok((m, s)),
        _ => Err("usage: band_capture --monitor <i> --secs <n>".to_string()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&args) {
        Ok((monitor, secs)) => {
            let _ = (monitor, secs);
            println!("not ready: K2a.11");
            1
        }
        Err(e) => {
            eprintln!("band_capture: {e}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<(u32, u32), String> {
        parse(&line.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn band_capture_parses_its_line() {
        assert_eq!(p("--monitor 0 --secs 60"), Ok((0, 60)));
        assert_eq!(p("--secs 5 --monitor 2"), Ok((2, 5)));
        for bad in
            ["", "--monitor 0", "--monitor x --secs 1", "--monitor 0 --secs 0", "--monitor 0 --monitor 1", "--fast"]
        {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
