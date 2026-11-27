//! Class icons from a real shell host (IC3): .txt at 16 px arrives as a ClassIcon frame and fills the cache.
// The shell host's test hooks (TestHooks, FILES_TEST_*) exist in debug builds only.
#![cfg(debug_assertions)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::decor::{ClassIconSource, ClassIcons, IconRef};
use files_engine::shell_client::{ShellClient, ShellConfig, ShellReply};
use oled_trust::frame::files::ShellEvt;

#[test]
fn host_source_end_to_end() {
    let root = TempRoot::new("ci-host");
    let client = Arc::new(ShellClient::new(
        ShellConfig { exe: common::files_exe(), test: Some(common::hooks(&root)) },
        Arc::new(|| {}),
    ));
    let icons = ClassIcons::new(
        ClassIconSource::ShellHost,
        root.path().join("data"),
        Some(Arc::clone(&client)),
        Arc::new(|| {}),
    );
    assert!(matches!(icons.get(b"txt", 16), IconRef::Pending));
    let (mut frame, mut ours) = (None, 0);
    let end = Instant::now() + Duration::from_secs(60);
    let got = loop {
        for r in client.drain() {
            if let ShellReply::Evt(ShellEvt::ClassIcon { ext, px, .. }) = &r {
                frame = Some((ext.to_string(), px.w, px.h));
            }
            ours += usize::from(icons.on_shell_reply(&r));
        }
        if let IconRef::Ready(a) = icons.get(b"txt", 16) {
            break a;
        }
        assert!(Instant::now() < end, "timed out; replies of ours so far: {ours}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(frame, Some(("txt".to_string(), 16, 16)), "a ClassIcon frame came from the host");
    assert_eq!(got.len(), 16 * 16 * 4);
    assert!(got.chunks_exact(4).any(|p| p.get(3).copied().unwrap_or(0) != 0), "not blank");
    assert!(ours >= 2, "the frame and the ReqDone were ours");
    client.shutdown();
}
