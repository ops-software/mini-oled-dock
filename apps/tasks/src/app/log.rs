//! tasks.log and tasks.log.1 in the local folder (F-T2). Lines carry counts, codes and states only.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::JoinHandle;

use oled_sys::logger::LogSink;
use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

use super::paths::Paths;

/// A log file larger than this is renamed to tasks.log.1 when it is opened.
const ROTATE_ABOVE: u64 = 512 * 1024;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

static SINK: LogSink = LogSink {
    path: log_path,
    open_append,
    debug_out: oled_sys::datadir::debug_out,
    version: env!("CARGO_PKG_VERSION"),
    on_line: no_line,
};

fn log_path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

fn no_line() {}

fn open_append(path: &Path) -> Option<File> {
    open_rotating(path, ROTATE_ABOVE)
}

/// Appends to `path`, after moving a file above `limit` bytes to path.1 (the old .1 is replaced). Reparse points are
/// not followed.
pub fn open_rotating(path: &Path, limit: u64) -> Option<File> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > limit) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let old = PathBuf::from(old);
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
    OpenOptions::new().create(true).append(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0).open(path).ok()
}

/// Starts the log thread; the handle goes to oled_sys::logger::stop. Without a local folder the lines go to the
/// debugger output.
pub fn start(p: &Paths) -> Option<JoinHandle<()>> {
    if let Some(dir) = &p.local {
        let _ = LOG_PATH.set(dir.join("tasks.log"));
    }
    oled_sys::logger::start("", &SINK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tasks-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rotation_at_512_kib() {
        let d = temp("rot");
        let f = d.join("tasks.log");
        // At the limit: appended to, not rotated.
        std::fs::write(&f, vec![b'a'; ROTATE_ABOVE as usize]).unwrap();
        writeln!(open_append(&f).unwrap(), "x").unwrap();
        assert!(!d.join("tasks.log.1").exists());
        // Above it: the old file becomes .1 and a new file starts.
        writeln!(open_append(&f).unwrap(), "y").unwrap();
        assert_eq!(std::fs::metadata(d.join("tasks.log.1")).unwrap().len(), ROTATE_ABOVE + 2);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "y\n");
        // A second rotation replaces the .1.
        std::fs::write(&f, vec![b'b'; ROTATE_ABOVE as usize + 1]).unwrap();
        let _ = open_append(&f).unwrap();
        assert_eq!(std::fs::metadata(d.join("tasks.log.1")).unwrap().len(), ROTATE_ABOVE + 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
