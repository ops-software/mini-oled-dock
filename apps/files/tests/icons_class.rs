//! Class icon extraction (IC2): read-only shell calls on a thread with COM; nothing is shown.
use files_engine::decor::extract::class_icon;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

/// Runs f on its own thread with an STA, as the decor thread and the shell host's image STA do.
fn on_sta<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(move || {
        // SAFETY: one init and one uninit on this thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let v = f();
        // SAFETY: pairs the init above.
        unsafe { CoUninitialize() };
        v
    })
    .join()
    .expect("the STA thread")
}

fn drawn(b: &[u8]) -> bool {
    b.chunks_exact(4).any(|p| p.get(3).copied().unwrap_or(0) != 0)
}

#[test]
fn txt_icon_16_and_24() {
    let (a, b) = on_sta(|| (class_icon("txt", 16), class_icon("txt", 24)));
    let (a, b) = (a.expect("a 16 px icon"), b.expect("a 24 px icon"));
    assert_eq!((a.len(), b.len()), (16 * 16 * 4, 24 * 24 * 4), "the asked sizes");
    assert!(drawn(&a) && drawn(&b), "not blank");
}

#[test]
fn folder_icon() {
    let f = on_sta(|| class_icon("", 16)).expect("a folder icon");
    assert_eq!(f.len(), 16 * 16 * 4);
    assert!(drawn(&f));
    let t = on_sta(|| class_icon("txt", 16)).expect("txt");
    assert_ne!(f, t, "a folder does not look like a text file");
}

#[test]
fn unknown_extension_and_bad_input() {
    let g = on_sta(|| class_icon("zzqqxx", 32)).expect("the generic icon");
    assert_eq!(g.len(), 32 * 32 * 4);
    assert!(on_sta(|| class_icon("txt", 0)).is_none());
    assert!(on_sta(|| class_icon("txt", 257)).is_none());
    assert!(on_sta(|| class_icon("a.b", 16)).is_none());
    assert!(on_sta(|| class_icon("a\\b", 16)).is_none());
}

#[test]
fn odd_sizes_come_out_exact() {
    for px in [20u16, 40, 64] {
        let v = on_sta(move || class_icon("txt", px)).expect("an icon");
        assert_eq!(v.len(), usize::from(px) * usize::from(px) * 4, "{px}");
    }
}

/// The lookup is a shell call that can fail for a moment when many threads ask at once; the app must still get an icon.
#[test]
fn generic_icon_survives_concurrent_callers() {
    let hs: Vec<_> = (0..24)
        .map(|_| {
            std::thread::spawn(|| {
                on_sta(|| (0..20).all(|_| class_icon("zzqqxx", 32).is_some() && class_icon("txt", 16).is_some()))
            })
        })
        .collect();
    for h in hs {
        assert!(h.join().expect("a caller"), "an icon was missing under load");
    }
}
