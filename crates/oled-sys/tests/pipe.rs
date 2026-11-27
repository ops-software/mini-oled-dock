//! WK2: named pipes, peers and the image rule (F-P3). Every name is random, so the tests run side by side.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oled_sys::ident::same_image;
use oled_sys::pipe::*;

const WAIT: Duration = Duration::from_secs(10);
const CAP: usize = 64 * 1024;

fn name() -> PipeName {
    PipeName::new("oled-test", "pipe", Some(&nonce())).unwrap()
}

fn server(name: &PipeName) -> Server {
    Server::create(name, &[current_user_sid().unwrap()]).unwrap()
}

/// A connected pair: (the server's end, the client's end).
fn pair() -> (Conn, Conn) {
    let n = name();
    let s = server(&n);
    let client = connect(&n, WAIT).unwrap();
    (s.accept(WAIT).unwrap(), client)
}

/// A client end opened as a plain file, to write bytes no Conn would.
fn raw_client(name: &PipeName) -> std::fs::File {
    std::fs::OpenOptions::new().read(true).write(true).open(name.full()).unwrap()
}

/// A folder of this test's, removed at drop.
struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Temp {
        let p = std::env::temp_dir().join(format!("oled-sys-{tag}-{}", nonce()));
        std::fs::create_dir_all(&p).unwrap();
        Temp(p)
    }
    /// A copy of this test exe in the folder.
    fn copy_of_exe(&self) -> PathBuf {
        let copy = self.0.join("copy.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
        copy
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn names_are_checked() {
    assert_eq!(PipeName::new("tasks", "agent", None).unwrap().full(), r"\\.\pipe\mini-oled-dock.tasks.agent");
    assert_eq!(
        PipeName::new("dock", "agent", Some("0a-f")).unwrap().full(),
        r"\\.\pipe\mini-oled-dock.dock.agent.0a-f"
    );
    let long = "b".repeat(33);
    let bad = [("Tasks", "a", None), ("", "a", None), ("a", "b.c", None), ("a", "b", Some("x/y")), ("a", &long, None)];
    for (app, purpose, n) in bad.into_iter().chain([("a", "b", Some(""))]) {
        assert!(matches!(PipeName::new(app, purpose, n), Err(PipeError::Name)), "{app} {purpose} {n:?}");
    }
    assert!(PipeName::new(&"a".repeat(32), "b", Some(&nonce())).is_ok());
}

#[test]
fn nonce_is_32_hex_and_differs() {
    let (a, b) = (nonce(), nonce());
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "{a}");
    assert_ne!(a, b);
}

#[test]
fn a_second_server_of_one_name_is_squatted() {
    let n = name();
    let _first = server(&n);
    assert!(matches!(Server::create(&n, &[current_user_sid().unwrap()]), Err(PipeError::Squatted)));
}

#[test]
fn frames_round_trip_and_caps() {
    let big = vec![0xA5u8; CAP];
    let frames = vec![b"x".to_vec(), big.clone(), Vec::new(), vec![1u8; CAP + 1]];
    // Client to server; the writer has its own thread, as a frame larger than the pipe's buffer waits for the reader.
    let (mut srv, client) = pair();
    let sent = frames.clone();
    let writer = std::thread::spawn(move || {
        let mut client = client;
        let results: Vec<bool> = sent.iter().map(|f| client.send(f).is_ok()).collect();
        (client, results)
    });
    for f in &frames[..3] {
        assert_eq!(&srv.recv(CAP, Some(WAIT)).unwrap(), f);
    }
    assert!(matches!(srv.recv(CAP, Some(WAIT)), Err(PipeError::TooLarge(n)) if n == CAP + 1));
    assert!(matches!(srv.recv(CAP, Some(WAIT)), Err(PipeError::Closed)), "a cut stream is not read again");
    drop(srv);
    let (mut client, results) = writer.join().unwrap();
    assert_eq!(&results[..3], [true; 3]);
    assert!(matches!(client.send(&vec![0u8; FRAME_MAX + 1]), Err(PipeError::TooLarge(n)) if n == FRAME_MAX + 1));
    // Server to client.
    let (srv, mut client) = pair();
    let back = frames[..3].to_vec();
    let writer = std::thread::spawn(move || {
        let mut srv = srv;
        back.iter().all(|f| srv.send(f).is_ok())
    });
    for f in &frames[..3] {
        assert_eq!(&client.recv(CAP, Some(WAIT)).unwrap(), f);
    }
    assert!(writer.join().unwrap());
    // A length prefix of 0xFFFFFFFF, written raw: refused before anything is allocated for it.
    let n = name();
    let s = server(&n);
    let mut raw = raw_client(&n);
    raw.write_all(&[0xFF; 4]).unwrap();
    let mut srv = s.accept(WAIT).unwrap();
    assert!(matches!(srv.recv(FRAME_MAX, Some(WAIT)), Err(PipeError::TooLarge(0xFFFF_FFFF))));
    // A client that went away reads as Closed.
    let (mut srv, client) = pair();
    drop(client);
    assert!(matches!(srv.recv(16, Some(WAIT)), Err(PipeError::Closed)));
}

#[test]
fn accept_times_out() {
    let n = name();
    let s = server(&n);
    let t = Instant::now();
    assert!(matches!(s.accept(Duration::from_millis(50)), Err(PipeError::Timeout)));
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(45) && took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn recv_times_out() {
    let (mut srv, mut client) = pair();
    let t = Instant::now();
    assert!(matches!(srv.recv(16, Some(Duration::from_millis(50))), Err(PipeError::Timeout)));
    assert!(t.elapsed() >= Duration::from_millis(45));
    client.send(b"late").unwrap();
    assert_eq!(srv.recv(16, Some(WAIT)).unwrap(), b"late", "a timeout leaves the connection usable");
    // A timeout in the middle of a frame keeps what was read: the next receive finishes the frame.
    let n = name();
    let s = server(&n);
    let mut raw = raw_client(&n);
    let mut srv = s.accept(WAIT).unwrap();
    raw.write_all(&[5, 0]).unwrap();
    assert!(matches!(srv.recv(16, Some(Duration::from_millis(50))), Err(PipeError::Timeout)));
    raw.write_all(&[0, 0, b'h', b'e']).unwrap();
    assert!(matches!(srv.recv(16, Some(Duration::from_millis(50))), Err(PipeError::Timeout)));
    raw.write_all(b"llo").unwrap();
    assert_eq!(srv.recv(16, Some(WAIT)).unwrap(), b"hello");
}

#[test]
fn client_and_server_peers() {
    let (srv, client) = pair();
    let exe = std::env::current_exe().unwrap();
    let own = peer_of_pid(std::process::id()).unwrap();
    for peer in [srv.client().unwrap(), client.server().unwrap()] {
        assert_eq!(peer.pid, std::process::id());
        assert!(same_image(&peer.image, &exe), "{}", peer.image.display());
        assert_eq!((peer.session, peer.integrity, peer.created), (own.session, own.integrity, own.created));
        assert!(matches!(peer.integrity, Integrity::Medium | Integrity::High), "{:?}", peer.integrity);
        assert!(peer.created > 0);
    }
    assert!(peer_of_pid(0).is_err(), "the idle process has no peer");
    assert_eq!(user_sid_of_pid(std::process::id()).unwrap(), current_user_sid().unwrap());
}

#[test]
fn a_rule_refuses_another_image() {
    let (srv, _client) = pair();
    let peer = srv.client().unwrap();
    let tmp = Temp::new("rule");
    let copy = tmp.copy_of_exe();
    let own = PeerRule {
        image: Some(std::env::current_exe().unwrap()),
        same_session: true,
        max_integrity: Some(Integrity::High),
        min_integrity: Some(Integrity::Medium),
        pid: Some(std::process::id()),
    };
    Conn::check(&peer, &own).unwrap();
    let refused = |rule: PeerRule| matches!(Conn::check(&peer, &rule), Err(PipeError::Peer(_)));
    assert!(refused(PeerRule { image: Some(copy.clone()), ..own.clone() }), "a copy of the image");
    assert!(refused(PeerRule { pid: Some(std::process::id() ^ 4), ..own.clone() }), "another pid");
    assert!(refused(PeerRule { max_integrity: Some(Integrity::Low), ..own.clone() }));
    assert!(refused(PeerRule { min_integrity: Some(Integrity::System), ..own.clone() }));
    let elsewhere = Peer { session: peer.session + 1, ..peer.clone() };
    assert!(matches!(Conn::check(&elsewhere, &own), Err(PipeError::Peer(_))), "another session");
}

#[test]
fn same_image_rules() {
    let exe = std::env::current_exe().unwrap();
    assert!(same_image(&exe, &exe));
    assert!(same_image(&exe, Path::new(&exe.to_string_lossy().to_uppercase())));
    assert!(same_image(&exe, Path::new(&format!(r"\\?\{}", exe.display()))));
    let tmp = Temp::new("image");
    let copy = tmp.copy_of_exe();
    let link = tmp.0.join("link.exe");
    std::fs::hard_link(&copy, &link).unwrap();
    assert!(same_image(&copy, &link), "a hard link is the same file");
    assert!(!same_image(&exe, &copy), "a copy is not");
    let missing = tmp.0.join("missing.exe");
    assert!(!same_image(&exe, &missing));
    assert!(!same_image(&missing, &missing));
}
