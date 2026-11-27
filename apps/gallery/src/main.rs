//! gallery.exe (P2-kit K0.2): parses the command line and runs one mode. Complete in K0.2 and never edited again: each
//! mode lives in its own module.

use gallery::cli::{self, Cli};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match cli::parse(&args) {
        Ok(Cli::Window(o)) => gallery::app::run(&o),
        Ok(Cli::Preview(o)) => gallery::preview::run(&o),
        Ok(Cli::Measure(o)) => gallery::measure::run(&o),
        Ok(Cli::Bench(filter)) => gallery::bench::run_all(filter.as_deref()),
        Err(e) => {
            eprintln!("gallery: {e}");
            eprintln!("{}", cli::USAGE);
            2
        }
    };
    std::process::exit(code);
}
