//! T6b.1, the fault switch of the live chord matrix: the script's parser and reader (pure, in every build: the module
//! is compiled here from its file, since tasks has it only with the fault-inject feature) and, in a fault-inject build,
//! what no-foreground does to a chord start (through the kit Harness: no window, no process).

#[allow(dead_code)] // at_start, after_first_paint and the rest are app::main's and the screen's
#[path = "../src/app/fault.rs"]
mod fault;

use std::path::PathBuf;
use std::time::Duration;

use fault::{front_allowed_under, parse, read, sleep_after_paint, Fault, FILE_NAME, MAX_BYTES, MAX_MS};

fn utf16le(s: &str) -> Vec<u8> {
    let mut b = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        b.extend(u.to_le_bytes());
    }
    b
}

#[test]
fn fault_parse() {
    assert_eq!(parse(b"hang-before-show 10000"), Some(Fault::HangBeforeShow { ms: 10_000 }));
    assert_eq!(parse(b"crash-at-start\r\n"), Some(Fault::CrashAtStart));
    assert_eq!(parse(b"hang-after-show 30000\n"), Some(Fault::HangAfterShow { ms: 30_000 }));
    assert_eq!(parse(b"slow-show 1200"), Some(Fault::SlowShow { ms: 1_200 }));
    assert_eq!(parse(b"no-foreground"), Some(Fault::NoForeground));
    // Spacing, the line end, only the first line, the BOMs.
    assert_eq!(parse(b"  slow-show \t 1200  \nno-foreground\n"), Some(Fault::SlowShow { ms: 1_200 }));
    assert_eq!(parse(b"\xEF\xBB\xBFno-foreground\r\n"), Some(Fault::NoForeground));
    assert_eq!(parse(&utf16le("crash-at-start\r\n")), Some(Fault::CrashAtStart));
    assert_eq!(parse(&utf16le("hang-after-show 5000")), Some(Fault::HangAfterShow { ms: 5_000 }));
    assert_eq!(parse(format!("slow-show {MAX_MS}").as_bytes()), Some(Fault::SlowShow { ms: MAX_MS }));
    assert_eq!(parse(b"slow-show 1"), Some(Fault::SlowShow { ms: 1 }));
    // Every word reads back as its fault.
    for f in [
        Fault::HangBeforeShow { ms: 7 },
        Fault::CrashAtStart,
        Fault::HangAfterShow { ms: 7 },
        Fault::SlowShow { ms: 7 },
        Fault::NoForeground,
    ] {
        let line = match f {
            Fault::CrashAtStart | Fault::NoForeground => f.word().to_string(),
            _ => format!("{} 7", f.word()),
        };
        assert_eq!(parse(line.as_bytes()), Some(f), "{line}");
    }
    // Anything else changes nothing.
    let bad: [&[u8]; 22] = [
        b"",
        b"\n",
        b"   ",
        b"Crash-At-Start",
        b"crash-at-start now",
        b"crash-at-start 5",
        b"no-foreground 5",
        b"# no-foreground",
        b"hang-after-show",
        b"hang-after-show 0",
        b"hang-after-show 600001",
        b"hang-after-show 99999999999",
        b"hang-after-show -5",
        b"hang-after-show +5",
        b"hang-after-show 1e3",
        b"hang-after-show 5 5",
        b"hang-before-show 5s",
        b"slow-show\n1200",
        b"\nno-foreground",
        b"\xC3\x28no-foreground",
        b"\xFF\xFE\x00",
        b"\xFF\xFE\x00\xD8",
    ];
    for b in bad {
        assert_eq!(parse(b), None, "{:?}", String::from_utf8_lossy(b));
    }
    // Only the first MAX_BYTES bytes count: a first line past them is cut, junk after a whole first line is not read.
    let late = format!("{}no-foreground", " ".repeat(MAX_BYTES - 4));
    assert_eq!(parse(late.as_bytes()), None);
    let mut long = b"no-foreground\n".to_vec();
    long.extend(std::iter::repeat_n(b'x', 4096));
    assert_eq!(parse(&long), Some(Fault::NoForeground));
}

#[test]
fn what_each_fault_does_after_the_first_paint() {
    assert_eq!(sleep_after_paint(Some(Fault::HangAfterShow { ms: 30_000 })), Some(Duration::from_secs(30)));
    for f in [None, Some(Fault::SlowShow { ms: 5 }), Some(Fault::NoForeground), Some(Fault::CrashAtStart)] {
        assert_eq!(sleep_after_paint(f), None, "{f:?}");
    }
    assert!(!front_allowed_under(Some(Fault::NoForeground)));
    for f in [None, Some(Fault::HangAfterShow { ms: 5 }), Some(Fault::SlowShow { ms: 5 })] {
        assert!(front_allowed_under(f), "{f:?}");
    }
}

#[test]
fn the_script_is_read_capped() {
    let dir = std::env::temp_dir().join(format!("tasks-fault-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(FILE_NAME);
    assert_eq!(read(&path), None, "no script");
    std::fs::write(&path, "hang-after-show 5000\n").unwrap();
    assert_eq!(read(&path), Some(Fault::HangAfterShow { ms: 5_000 }));
    let mut big = b"no-foreground\n".to_vec();
    big.resize(1 << 20, b'x');
    std::fs::write(&path, &big).unwrap();
    assert_eq!(read(&path), Some(Fault::NoForeground), "a large file is read only to its first bytes");
    std::fs::write(&path, "slow-show 1200 ms\n").unwrap();
    assert_eq!(read(&path), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_script_lives_in_the_tasks_folder() {
    let p: PathBuf = fault::script_path().expect("the local folder of this user");
    assert!(p.ends_with("mini-oled-dock\\tasks\\fault.txt"), "{}", p.display());
}

/// A fault-inject build under no-foreground: a chord start paints and never asks the kit for the front; the dock's own
/// steps have to raise it (LIVE-T6b case 8).
#[cfg(feature = "fault-inject")]
#[test]
fn no_foreground_skips_the_front_request() {
    use std::sync::Arc;
    use tasks::app::exec::RecordingExecutor;
    use tasks::app::fault as live;
    use tasks::app::launch::{Launcher, RecordingLauncher};
    use tasks::app::Deps;
    use tasks::ui::screen::TasksScreen;

    assert!(live::install(Some(live::Fault::NoForeground)), "this test binary sets the fault once");
    assert!(!live::front_allowed());
    let l: Arc<dyn Launcher> = Arc::new(RecordingLauncher::default());
    let mut deps = Deps::inert(oled_ui::UiLang::EN, l, Arc::new(RecordingExecutor::default()));
    deps.show = oled_trust::frame::tasks::Show { from_chord: true, ..Default::default() };
    let mut h = oled_ui::harness::Harness::new(
        TasksScreen::new(deps),
        oled_ui::SizeL { w: 640, h: 560 },
        96,
        oled_ui::Tokens::oled(),
    );
    h.render();
    h.advance(10);
    h.send(Box::new(tasks::app::presence::PresenceMsg { minimised: false, cloaked: false }));
    h.advance(1000);
    assert!(h.window_requests().is_empty(), "no front request under no-foreground");
}
