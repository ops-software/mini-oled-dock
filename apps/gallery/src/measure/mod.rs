//! The measure modes of the live sessions (P2-kit section 5): the dispatch table, complete in K0.2; one file per mode,
//! filled by its owner.

pub mod anim;
pub mod band;
pub mod caption;
pub mod empty;
pub mod idle;
pub mod leak;
pub mod monitors;
pub mod theme;

use crate::cli::MeasureOpts;

/// A measure mode (--list-monitors is Monitors).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Empty,
    Idle,
    Leak,
    Theme,
    Band,
    Anim,
    Caption,
    Monitors,
}

impl Mode {
    /// The mode of a --measure value.
    pub fn from_name(name: &str) -> Option<Mode> {
        Some(match name {
            "empty" => Mode::Empty,
            "idle" => Mode::Idle,
            "leak" => Mode::Leak,
            "theme" => Mode::Theme,
            "band" => Mode::Band,
            "anim" => Mode::Anim,
            "caption" => Mode::Caption,
            _ => return None,
        })
    }

    /// The --measure value of a mode ("list-monitors" for Monitors).
    pub fn name(self) -> &'static str {
        match self {
            Mode::Empty => "empty",
            Mode::Idle => "idle",
            Mode::Leak => "leak",
            Mode::Theme => "theme",
            Mode::Band => "band",
            Mode::Anim => "anim",
            Mode::Caption => "caption",
            Mode::Monitors => "list-monitors",
        }
    }
}

/// Runs a measure mode; its exit code.
pub fn run(opts: &MeasureOpts) -> i32 {
    match opts.mode {
        Mode::Empty => empty::run(opts),
        Mode::Idle => idle::run(opts),
        Mode::Leak => leak::run(opts),
        Mode::Theme => theme::run(opts),
        Mode::Band => band::run(opts),
        Mode::Anim => anim::run(opts),
        Mode::Caption => caption::run(opts),
        Mode::Monitors => monitors::run(opts),
    }
}

/// What a mode, page or bench that is not filled yet prints: "not ready: <task>"; exit code 1.
pub fn not_ready(task: &str) -> i32 {
    println!("not ready: {task}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_names_round_trip() {
        for m in [Mode::Empty, Mode::Idle, Mode::Leak, Mode::Theme, Mode::Band, Mode::Anim, Mode::Caption] {
            assert_eq!(Mode::from_name(m.name()), Some(m));
        }
        assert_eq!(Mode::from_name("list-monitors"), None);
    }
}
