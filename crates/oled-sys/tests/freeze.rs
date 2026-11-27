//! PF1: one line per frozen item of the plumbing plan (F-P3, F-P4, F-P5 OS side, F-P7, F-P11, F-P12) with its full
//! signature, so a change fails to compile.
// Function-pointer types spelled out in full are the point of this file.
#![allow(clippy::type_complexity)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oled_sys::pipe::*;
use oled_sys::worker::*;
use oled_sys::{datadir, ident, journal_os, lang, selftest, sha256};
use oled_trust::journal::{Journal, OsErr, Scope};

#[test]
fn pipe_items_keep_their_signatures() {
    let _: usize = FRAME_MAX;
    let _a: fn(&str, &str, Option<&str>) -> Result<PipeName, PipeError> = PipeName::new;
    let _b: fn(&PipeName) -> String = PipeName::full;
    let _c: fn() -> String = nonce;
    let _d: fn(&PipeName, &[String]) -> Result<Server, PipeError> = Server::create;
    let _e: fn(Server, Duration) -> Result<Conn, PipeError> = Server::accept;
    let _f: fn(&PipeName, Duration) -> Result<Conn, PipeError> = connect;
    let _g: fn(&Conn) -> Result<Peer, PipeError> = Conn::client;
    let _h: fn(&Conn) -> Result<Peer, PipeError> = Conn::server;
    let _i: fn(&Peer, &PeerRule) -> Result<(), PipeError> = Conn::check;
    let _j: fn(&mut Conn, &[u8]) -> Result<(), PipeError> = Conn::send;
    let _k: fn(&mut Conn, usize, Option<Duration>) -> Result<Vec<u8>, PipeError> = Conn::recv;
    let _l: fn(u32) -> Result<Peer, PipeError> = peer_of_pid;
    let _m: fn(u32) -> Result<String, PipeError> = user_sid_of_pid;
    let _n: fn() -> Result<String, PipeError> = current_user_sid;
    assert!(Integrity::System > Integrity::Medium && Integrity::Low > Integrity::Untrusted);
    let _ = Peer { pid: 0, image: PathBuf::new(), session: 0, integrity: Integrity::High, created: 0 };
    let _ = PeerRule::default().max_integrity.or(PeerRule::default().min_integrity);
    let _ = (
        PipeError::Name,
        PipeError::Squatted,
        PipeError::Timeout,
        PipeError::Closed,
        PipeError::TooLarge(0),
        PipeError::Peer(String::new()),
    );
}

#[test]
fn worker_items_keep_their_signatures() {
    let _a: fn(fn()) -> bool = oled_sys::worker::bound::set_on_thread;
    let _b: fn(&WorkerSpec) -> Result<Worker, WorkerError> = spawn;
    let _c: fn(&WorkerSpec) -> Result<Worker, WorkerError> = reconnect;
    let _d: fn(&Worker) -> u32 = Worker::pid;
    let _e: fn(&Worker) -> windows::Win32::Foundation::HANDLE = Worker::process;
    let _f: fn(&mut Worker, &[u8]) -> Result<(), WorkerError> = Worker::send;
    let _g: fn(&mut Worker, Duration) -> Result<Vec<u8>, WorkerError> = Worker::recv;
    let _h: fn(&mut Worker) = Worker::kill;
    let _i: fn(&mut Worker) -> Option<u32> = Worker::try_wait;
    let _j: fn(Breaker) -> BreakerState = BreakerState::new;
    let _k: fn(&BreakerState, Instant) -> bool = BreakerState::allow;
    let _l: fn(&mut BreakerState, Instant) = BreakerState::failed;
    let _m: fn(&BreakerState, Instant) -> bool = BreakerState::is_open;
    let _n: fn() -> host::Host = host::bound;
    let _o: fn(&str, &str, &Path) -> Result<host::Host, WorkerError> = host::detached;
    let _p: fn(&mut host::Host, usize) -> Result<Option<Vec<u8>>, WorkerError> = host::Host::recv;
    let _q: fn(&mut host::Host, &[u8]) -> Result<(), WorkerError> = host::Host::send;
    let _r: fn(&mut host::Host, Duration) -> Result<(), WorkerError> = host::Host::wait_client;
    assert_eq!(Breaker::DEFAULT.max_restarts, 3);
    assert_eq!(Breaker::DEFAULT.window, Duration::from_secs(60));
    let _ = (WorkerMode::Bound, WorkerMode::Detached);
    let _ = Deadlines { start: Duration::ZERO, request: Duration::ZERO, heartbeat: None };
    let _ = (
        WorkerError::Untrusted(String::new()),
        WorkerError::Deadline,
        WorkerError::BreakerOpen,
        WorkerError::Exited(None),
    );
}

#[test]
fn journal_os_items_keep_their_signatures() {
    let _a: fn(Scope) -> journal_os::RealOs = journal_os::RealOs::for_scope;
    let _b: fn(journal_os::FileSink, &Journal) -> Result<bool, OsErr> = journal_os::FileSink::delete_if_settled;
    let _c: fn(Scope) -> Option<PathBuf> = journal_os::path;
    let _d: fn() -> Option<PathBuf> = journal_os::update_journal_path;
    let _e: fn(Scope, Duration) -> Result<(Journal, journal_os::FileSink), OsErr> = journal_os::open_locked;
    let _f: fn(&Path, Scope, Duration) -> Result<(Journal, journal_os::FileSink), OsErr> = journal_os::open_locked_at;
    let _g: fn(Scope) -> Result<Journal, String> = journal_os::load;
    let _h: fn(Scope, bool) -> bool = journal_os::scope_allowed;
    fn _real_os_is_a_journal_os(_: &dyn oled_trust::journal::JournalOs) {}
    fn _file_sink_is_a_sink(_: &mut dyn oled_trust::journal::Sink) {}
    let _ = (
        _real_os_is_a_journal_os as fn(&dyn oled_trust::journal::JournalOs),
        _file_sink_is_a_sink as fn(&mut dyn oled_trust::journal::Sink),
    );
}

#[test]
fn selftest_items_keep_their_signatures() {
    let _a: fn(&str, &str, &str) -> String = selftest::event_name;
    let _b: fn(&str, &str, &str) -> std::io::Result<()> = selftest::signal;
    let _c: fn(&str, &str, &str) -> Result<(selftest::Waiter, String), PipeError> = selftest::Waiter::create;
    let _d: fn(&selftest::Waiter, Duration) -> bool = selftest::Waiter::wait;
    let _e: fn(&str, bool, &Path, (u32, u32, u32)) -> u32 = selftest::notify_updated;
    assert_eq!(selftest::UPDATED_MESSAGE, "mini-oled-dock.Updated");
}

#[test]
fn lang_items_keep_their_signatures() {
    assert_eq!(lang::LANG_MAGIC, *b"OLNG");
    assert_eq!((lang::LANG_FORMAT, lang::LANG_MAX_BYTES, lang::LANG_MAX_KEYS), (1, 1024 * 1024, 16_384));
    let _a: fn(Vec<u8>) -> Result<lang::LangFile, lang::LangError> = lang::LangFile::parse;
    let _b: fn(&lang::LangFile) -> &str = lang::LangFile::tag;
    let _c: fn(&lang::LangFile) -> (u16, u16, u16) = lang::LangFile::suite_version;
    let _d: fn(&lang::LangFile) -> usize = lang::LangFile::len;
    let _e: fn(&lang::LangFile, u64) -> Option<&str> = lang::LangFile::get;
    let _f: fn(&str, (u16, u16, u16), &[(u64, &str)]) -> Result<Vec<u8>, lang::LangError> = lang::encode;
    let _ = (lang::LangError::Magic, lang::LangError::Format(0), lang::LangError::TooLarge);
}

#[test]
fn moved_items_keep_their_signatures() {
    let _a: &str = datadir::ROOT_NAME;
    let _b: fn(Option<datadir::RootInfo>) -> bool = datadir::register;
    let _c: fn() -> Option<&'static datadir::RootInfo> = datadir::root;
    let _d: fn() -> bool = datadir::writable;
    let _e: fn() -> Option<PathBuf> = datadir::lookup_root;
    let _f: fn(bool) -> Option<datadir::RootInfo> = datadir::open_existing;
    let _f2: fn() -> Option<PathBuf> = datadir::suite_dir;
    let _f3: fn() -> Option<PathBuf> = datadir::journal_dir;
    let _g: fn(&Path, &[u8], datadir::FileAcl) -> Result<(), String> = datadir::write_atomic;
    let _h: fn(&Path) -> Option<std::fs::File> = datadir::open_append;
    let _i: fn(&Path, u64, bool) -> std::io::Result<Vec<u8>> = datadir::read_file;
    let _j: fn(&Path, bool) -> std::io::Result<std::fs::File> = datadir::open_checked;
    let _k: fn(&Path, bool) -> std::io::Result<std::fs::File> = datadir::open_checked_read_only;
    let _l: fn(&Path) -> bool = datadir::is_admin_only_dir;
    let _m: fn(&str) -> bool = datadir::is_plain_dir_name;
    let _n: fn(&windows::core::GUID) -> Option<PathBuf> = datadir::known_folder;
    let _o: fn() -> Option<String> = datadir::token_user_sid;
    let _p: fn(&str) = datadir::debug_out;
    let _q: fn(&Path, &Path) -> bool = ident::same_file;
    let _r: fn(&Path, &Path) -> bool = ident::same_image;
    let _s: fn(&[u8]) -> Result<[u8; 32], String> = sha256::sha256;
    let _ = (datadir::FileAcl::Inherit, datadir::FileAcl::UserEditable);
}
