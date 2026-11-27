//! The ops pipe (OP5a): a server and a client in one test process, each test on its own tagged name.

mod common;

use std::sync::Arc;
use std::time::Duration;

use files_engine::host::named::{
    client_check, connect, ops_pipe_name, own_peer, server_check, FrameIo, NamedError, NamedServer,
};
use oled_sys::pipe::Integrity;

fn name(tag: &str) -> String {
    let session = own_peer().expect("own token").session;
    ops_pipe_name(session, 1, "00000000", Some(tag)).unwrap()
}

fn me() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

#[test]
fn pipe_round_trip() {
    let n = name(&common::test_tag());
    let server = Arc::new(NamedServer::create(&n, &me(), false).unwrap());
    let s = server.clone();
    let echo = std::thread::spawn(move || {
        let mut conn = s.accept().unwrap();
        let mut seen = 0;
        while let Some(f) = conn.recv_frame().unwrap() {
            conn.send_frame(&f).unwrap();
            seen += 1;
        }
        seen
    });
    let mut client = connect(&n, Duration::from_secs(5), true).unwrap();
    assert_eq!(client.server_pid(), Some(std::process::id()));
    for i in 0..100usize {
        let frame: Vec<u8> = (0..(i * 997) % 200_000).map(|b| b as u8).collect();
        client.send_frame(&frame).unwrap();
        assert_eq!(client.recv_frame().unwrap(), Some(frame), "frame {i}");
    }
    drop(client);
    assert_eq!(echo.join().unwrap(), 100, "100 frames both ways, then the end");
}

#[test]
fn second_server_is_squatted() {
    let n = name(&common::test_tag());
    let _first = NamedServer::create(&n, &me(), false).unwrap();
    assert!(matches!(NamedServer::create(&n, &me(), false), Err(NamedError::Squatted)));
}

#[test]
fn oversize_length_refused() {
    let n = name(&common::test_tag());
    let server = Arc::new(NamedServer::create(&n, &me(), false).unwrap());
    let s = server.clone();
    let reader = std::thread::spawn(move || {
        let mut conn = s.accept().unwrap();
        let first = conn.recv_frame();
        let second = conn.recv_frame();
        (first.map_err(|e| e.kind()), second.map_err(|e| e.kind()))
    });
    let client = connect(&n, Duration::from_secs(5), true).unwrap();
    client.write_raw(&u32::MAX.to_le_bytes()).unwrap();
    client.write_raw(&[0u8; 64]).unwrap();
    let (first, second) = reader.join().unwrap();
    assert_eq!(first, Err(std::io::ErrorKind::InvalidData), "refused without allocating 4 GiB");
    assert_eq!(second, Ok(None), "the stream is poisoned");
}

#[test]
fn absent_pipe_is_absent() {
    let n = name(&common::test_tag());
    assert!(matches!(connect(&n, Duration::from_millis(200), false), Err(NamedError::Absent)));
    assert!(matches!(connect(&n, Duration::from_millis(50), true), Err(NamedError::Absent)));
}

#[test]
fn integrity_floor_both_ways() {
    let own = own_peer().unwrap().clone();
    let user = oled_sys::pipe::current_user_sid().unwrap();
    let image = own.image.clone();
    // The host admits no client below its own integrity (a lower process may run the same image).
    for (level, ok) in [(Integrity::Low, false), (Integrity::Untrusted, false), (own.integrity, true)] {
        let peer = oled_sys::pipe::Peer { integrity: level, ..own.clone() };
        assert_eq!(client_check(&peer, own.integrity, &image, false).is_ok(), ok, "client at {level:?}");
    }
    // The client admits only a server at exactly its integrity, as its user, running the host's image. The level above
    // is the next one up from this process's own: High for a test at Medium, System on a runner whose processes run
    // elevated.
    let above = if own.integrity >= Integrity::High { Integrity::System } else { Integrity::High };
    for (level, ok) in [(Integrity::Low, false), (above, false), (own.integrity, true)] {
        let peer = oled_sys::pipe::Peer { integrity: level, ..own.clone() };
        assert_eq!(server_check(&peer, &user, &user, own.integrity, &image).is_ok(), ok, "server at {level:?}");
    }
    let other = "S-1-5-21-1000000001-1000000002-1000000003-1001";
    assert!(server_check(&own, other, &user, own.integrity, &image).is_err(), "another user");
    assert!(server_check(&own, &user, &user, own.integrity, std::path::Path::new("C:\\x\\files.exe")).is_err());
}
