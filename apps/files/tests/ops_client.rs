//! OpsClient (OP6) against real host processes, each test with its own hooks (pipe tag, data folder, fence), and
//! against a fake host over the in-memory pair for the frame counts.

mod common;
mod ops_util;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::host::named::{mem_pair, FrameIo, NamedServer, FRAME_CAP};
use files_engine::ops_client::{OpsClient, OpsConfig, OpsError, OpsEvent};
use oled_trust::frame::files::{Conflicts, ItemRef, OpEvt, OpId, OpReq, OpResult, UndoKind, WidePath, MAX_LIST, OPS};
use oled_trust::frame::{decode, decode_hello, encode, encode_hello, SPLIT_MARGIN};
use ops_util::wp;

fn client(root: &TempRoot, hooks: files_engine::host::TestHooks) -> (OpsClient, Arc<AtomicU32>) {
    let n = Arc::new(AtomicU32::new(0));
    let k = n.clone();
    let cfg = OpsConfig { exe: common::files_exe(), data_dir: root.at("data"), owner: 0, test: Some(hooks) };
    (
        OpsClient::new(
            cfg,
            Arc::new(move || {
                k.fetch_add(1, Ordering::Relaxed);
            }),
        ),
        n,
    )
}

/// Drains until `op` finished; every event of the wait, in order.
fn until_finished(c: &OpsClient, op: OpId) -> Vec<OpsEvent> {
    let start = Instant::now();
    let mut all = Vec::new();
    loop {
        let evs = c.drain();
        let done = evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Finished { op: o, .. }) if *o == op));
        all.extend(evs);
        if done {
            return all;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "no Finished within 60 s: {all:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn three_files(root: &TempRoot) -> Vec<ItemRef> {
    ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| ItemRef::Path(wp(&root.file(&format!("src/{n}"), n.as_bytes()))))
        .collect()
}

#[test]
fn client_spawns_and_copies() {
    let root = TempRoot::new("client_copy");
    let (c, notified) = client(&root, common::hooks(&root));
    assert_eq!(c.host_pid(), None, "no process before the first request");
    let op = c.copy(&three_files(&root), &wp(&root.dir("dst")), Conflicts::AskSystem).unwrap();
    let evs = until_finished(&c, op);
    assert!(evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Accepted { op: o }) if *o == op)), "{evs:?}");
    assert!(
        evs.iter().any(|e| matches!(e, OpsEvent::Evt(OpEvt::Finished { result: OpResult::Done, items: 3, .. }))),
        "{evs:?}"
    );
    let undo = evs.iter().find_map(|e| match e {
        OpsEvent::Undo(r) => Some(r.clone()),
        _ => None,
    });
    let undo = undo.expect("the undo record, assembled");
    assert_eq!((undo.of, undo.kind, undo.entries.len()), (op, UndoKind::RecycleCopies, 3));
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(root.hash(&format!("dst/{n}")), root.hash(&format!("src/{n}")));
    }
    assert!(c.host_pid().is_some_and(|p| p != std::process::id()));
    assert!(notified.load(Ordering::Relaxed) > 0, "notify was called");
    c.shutdown();
}

/// A fake host over the in-memory pair: Hello, then every request decoded into `seen`, Accepted for operations.
fn fake_host(io: files_engine::host::named::MemIo) -> std::sync::mpsc::Receiver<OpReq> {
    let (tx, rx) = channel();
    let mut io = io;
    std::thread::spawn(move || {
        let mut hello = Vec::new();
        encode_hello(&OPS, files_engine::BUILD_ID, &mut hello);
        io.send_frame(&hello).unwrap();
        let theirs = io.recv_frame().unwrap().unwrap();
        decode_hello(&OPS, &theirs).unwrap();
        while let Ok(Some(f)) = io.recv_frame() {
            let (req, _) = decode::<OpReq>(&f).expect("every request decodes");
            if let OpReq::Copy { op, .. } = &req {
                let mut buf = Vec::new();
                encode(&OpEvt::Accepted { op: *op }, &mut buf).unwrap();
                io.send_frame(&buf).unwrap();
            }
            if tx.send(req).is_err() {
                break;
            }
        }
    });
    rx
}

fn stage_frames(rx: &std::sync::mpsc::Receiver<OpReq>) -> Vec<usize> {
    let mut sizes = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).expect("the requests arrive") {
            OpReq::Stage { items, .. } => sizes.push(items.len()),
            OpReq::Copy { .. } => return sizes,
            OpReq::Attach { .. } => {}
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn stage_chunking() {
    let (host_end, ui_end) = mem_pair();
    let rx = fake_host(host_end);
    let c = OpsClient::with_io(ui_end, Arc::new(|| {}));
    let items: Vec<ItemRef> =
        (0..3_000).map(|i| ItemRef::Path(WidePath::parse(&format!("C:\\src\\f{i}")).unwrap())).collect();
    c.copy(&items, &WidePath::parse("C:\\dst").unwrap(), Conflicts::AskSystem).unwrap();
    let sizes = stage_frames(&rx);
    assert!(sizes.len() >= 2, "at least two Stage frames: {sizes:?}");
    assert!(sizes.iter().all(|&n| n <= MAX_LIST as usize));
    assert_eq!(sizes.iter().sum::<usize>(), 3_000);
}

#[test]
fn stage_chunking_by_bytes() {
    let (host_end, ui_end) = mem_pair();
    let rx = fake_host(host_end);
    let counter = ui_end.clone();
    let c = OpsClient::with_io(ui_end, Arc::new(|| {}));
    let long = |i: u32| {
        let mut s = format!("C:\\{i:05}\\");
        s.extend(std::iter::repeat_n('p', 32_767 - s.len()));
        ItemRef::Path(WidePath::parse(&s).unwrap())
    };
    let items: Vec<ItemRef> = (0..500).map(long).collect();
    c.copy(&items, &WidePath::parse("C:\\dst").unwrap(), Conflicts::AskSystem).unwrap();
    let sizes = stage_frames(&rx);
    assert!(sizes.len() > 1, "several Stage frames: {sizes:?}");
    assert_eq!(sizes.iter().sum::<usize>(), 500);
    let limit = 6 + OPS.max_payload as usize - SPLIT_MARGIN as usize;
    for n in counter.sent_sizes() {
        assert!(n <= limit && n <= FRAME_CAP, "a frame of {n} bytes is over the limit {limit}");
    }
}

#[test]
fn squatter_refused() {
    let root = TempRoot::new("client_squatter");
    let hooks = common::hooks(&root);
    let name = files_engine::role_ops::pipe_name(hooks.pipe_tag.as_deref(), None).unwrap();
    let squatter = Arc::new(NamedServer::create(&name, &std::env::current_exe().unwrap(), false).unwrap());
    let (tx, rx) = channel();
    let s = squatter.clone();
    std::thread::spawn(move || {
        while let Ok(mut conn) = s.accept() {
            let got = conn.recv_frame().ok().flatten();
            let _ = tx.send(got.map(|f| f.len()));
            s.disconnect();
        }
    });
    let (c, _) = client(&root, hooks);
    let items = three_files(&root);
    let dst = wp(&root.dir("dst"));
    assert_eq!(c.copy(&items, &dst, Conflicts::AskSystem), Err(OpsError::Unavailable("ops host is not ours")));
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), None, "the squatter got no frame at all");
    assert_eq!(c.copy(&items, &dst, Conflicts::AskSystem), Err(OpsError::Unavailable("ops host is not ours")));
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "not tried again for 60 s");
    assert!(!root.at("dst/a.txt").exists());
}

#[test]
fn two_clients_two_hosts() {
    let root = TempRoot::new("client_two");
    let (a, _) = client(&root, common::hooks(&root));
    let (b, _) = client(&root, common::hooks(&root));
    let items = three_files(&root);
    let oa = a.copy(&items, &wp(&root.dir("dst_a")), Conflicts::AskSystem).unwrap();
    let ob = b.copy(&items, &wp(&root.dir("dst_b")), Conflicts::AskSystem).unwrap();
    until_finished(&a, oa);
    until_finished(&b, ob);
    let (pa, pb) = (a.host_pid().unwrap(), b.host_pid().unwrap());
    assert_ne!(pa, pb, "each client talks to its own host");
    for d in ["dst_a", "dst_b"] {
        assert_eq!(root.hash(&format!("{d}/a.txt")), root.hash("src/a.txt"));
    }
}

/// A nested path over 260 units copies through the host (files.exe is long-path aware by its manifest; the test
/// executables are not, which is why the in-process runner reports such a nested item as failed).
#[test]
fn host_copies_a_long_tree() {
    let root = TempRoot::new("client_long");
    let seg = |c: char| std::iter::repeat_n(c, 100).collect::<String>();
    let deep = format!("src/t/{}/{}/{}/f.txt", seg('x'), seg('y'), seg('z'));
    root.file(&deep, b"deep");
    root.file("src/t/a.txt", b"a");
    let (c, _) = client(&root, common::hooks(&root));
    let op = c.copy(&[ItemRef::Path(wp(&root.at("src/t")))], &wp(&root.dir("dst")), Conflicts::AskSystem).unwrap();
    let evs = until_finished(&c, op);
    let failed: Vec<_> = evs.iter().filter(|e| matches!(e, OpsEvent::Evt(OpEvt::ItemFailed { .. }))).collect();
    assert!(failed.is_empty(), "{failed:?}");
    // The copy engine may add a Zone.Identifier stream to a copy (seen on a long path): recorded, not asserted.
    let mut copied = root.hash_tree("dst/t");
    let marks: Vec<String> = copied.keys().filter(|k| k.ends_with(":Zone.Identifier")).cloned().collect();
    copied.retain(|k, _| !k.ends_with(":Zone.Identifier"));
    println!("streams the copy engine added: {}", marks.len());
    assert_eq!(copied, root.hash_tree("src/t"), "the whole tree, the long path included");
}
