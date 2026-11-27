//! The dock's log: the mechanism is oled_sys::logger, the files are datadir's.
use oled_sys::logger::LogSink;
pub use oled_sys::logger::{buffer_say, enable_stdout, log_line, say, stop, take_say_buffer};

fn on_line() {
    probe!(crate::probe::bump(crate::probe::C::LogLines));
}

static SINK: LogSink = LogSink {
    path: crate::datadir::log_path,
    open_append: crate::datadir::open_append,
    debug_out: crate::datadir::debug_out,
    version: env!("CARGO_PKG_VERSION"),
    on_line,
};

pub fn start(tag: &'static str) -> Option<std::thread::JoinHandle<()>> {
    oled_sys::logger::start(tag, &SINK)
}

/// The panic hook's write (main.rs install_panic_hook): synchronous, works before start.
pub fn write_sync(msg: &str) {
    oled_sys::logger::write_sync(&SINK, msg)
}

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => { $crate::logger::log_line(format_args!($($a)*)) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_dock_sink_names_the_dock_version() {
        assert_eq!(super::SINK.version, env!("CARGO_PKG_VERSION"));
    }
}
