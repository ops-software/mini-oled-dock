//! The Del flow against the real helper and the system's classifier, on the hosted runner of CI's files-disposable job
//! (P3-files-app F3.2): a file on the FAT32 VHD and one on the SMB share of files-disposable.ps1 open the bar; a local
//! NTFS file inside a TempRoot goes to the Recycle Bin and the undo restores it (RESTORE_FROM_BIN) or hands the bin
//! to File Explorer. Never on a developer's PC: every test starts with common::disposable().

mod common;

use std::rc::Rc;
use std::time::{Duration, Instant};

use common::temp::TempRoot;
use files_engine::bar;
use files_engine::fakes::{self, Fakes, FT_2026_11_21};
use files_engine::format::Fmt;
use files_engine::host::TestHooks;
use files_engine::listing::WidePath;
use files_engine::ops::consts::RESTORE_FROM_BIN;
use files_engine::ops_client::{OpsClient, OpsConfig, OpsError, OpsEvent, UndoRecord};
use files_engine::services::OpsService;
use files_engine::session::Session;
use files_engine::tr::{k, t};
use files_engine::types::TypeNames;
use files_engine::window::{FilesScreen, Shared, WinKind, WinRec};
use oled_trust::frame::files::{ItemRef, Location, OpId};
use oled_ui::harness::Harness;
use oled_ui::{SizeL, Tokens, UiLang};

/// The real client behind the window's seam.
struct RealOps(OpsClient);

impl OpsService for RealOps {
    fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.0.recycle(items)
    }
    fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.0.delete_permanent(items)
    }
    fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError> {
        self.0.rename(item, to)
    }
    fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError> {
        self.0.new_folder(parent, name)
    }
    fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError> {
        self.0.undo(rec)
    }
    fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError> {
        self.0.resume_rest(of)
    }
    fn cancel(&self, op: OpId) {
        self.0.cancel(op)
    }
    fn pause(&self, op: OpId, on: bool) {
        self.0.pause(op, on)
    }
    fn bring_forward(&self, op: OpId) {
        self.0.bring_forward(op)
    }
    fn ack_done(&self, of: OpId) {
        self.0.ack_done(of)
    }
    fn host_pid(&self) -> Option<u32> {
        self.0.host_pid()
    }
    fn drain(&self) -> Vec<OpsEvent> {
        self.0.drain()
    }
    fn shutdown(&self) {
        self.0.shutdown()
    }
}

struct Run {
    f: Fakes,
    shared: Rc<Shared>,
    h: Harness<FilesScreen>,
}

/// A window over the folder of the file, with the real client (its host in a TempRoot data folder, fenced to `fence`, a
/// FILES_TEST_ROOT list) and the system's classifier; the listing comes from the fake enumerator.
fn window(data: &TempRoot, dir: &WidePath, name: &str, fence: &str) -> Run {
    let f = Fakes::new();
    f.enumr.queue(dir, fakes::listing(&[(name, false, 100, FT_2026_11_21)]));
    let env = vec![("FILES_TEST_ROOT".into(), fence.into())];
    let hooks = TestHooks { pipe_tag: Some(common::test_tag()), build_tag: None, env };
    let cfg = OpsConfig { exe: common::files_exe(), data_dir: data.at("data"), owner: 0, test: Some(hooks) };
    let mut services = f.services();
    services.ops = Rc::new(RealOps(OpsClient::new(cfg, f.hub.notifier())));
    let shared =
        Shared::new(std::sync::Arc::clone(&f.hub), services, UiLang::EN, Session::default(), TypeNames::empty());
    *shared.fmt.borrow_mut() = Rc::new(Fmt::fixed_en());
    assert_eq!(shared.new_win(), 1);
    let at = Location::Fs(dir.clone());
    shared.windows.borrow_mut().push(WinRec {
        win: 1,
        kind: WinKind::Main,
        wref: None,
        hwnd: None,
        location: Some(at.clone()),
    });
    let tab = shared.new_tab();
    let screen = FilesScreen::new(Rc::clone(&shared), 1, at, None, tab);
    let mut h = Harness::new(screen, SizeL { w: 800, h: 500 }, 96, Tokens::oled());
    h.render();
    f.pump(&mut h, 1);
    h.render();
    // Everything selected: the one file.
    h.key("Ctrl+A");
    Run { f, shared, h }
}

impl Run {
    /// Pumps until the condition holds (the real helper answers on its own thread).
    fn until(&mut self, what: &str, mut ok: impl FnMut(&Run) -> bool) {
        let start = Instant::now();
        loop {
            self.f.pump(&mut self.h, 1);
            self.h.render();
            if ok(self) {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "{what}: not within 60 s");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
#[ignore = "disposable: CI files-disposable job only"]
fn disposable_del_flow_on_the_runner() {
    common::disposable();
    let data = TempRoot::new("disp_del_flow");
    // A FAT32 volume and a share: no Recycle Bin the classifier can vouch for, so Del asks first. The item need not
    // exist: the classification reads the volume, never the file's content, and nothing is sent before the bar.
    for var in ["FILES_DISP_FAT", "FILES_DISP_UNC"] {
        let base = std::env::var(var).unwrap_or_else(|_| panic!("{var} is set by files-disposable.ps1 -Setup"));
        let dir = WidePath::parse(&base).unwrap();
        let item = dir.join(&"report-final-v2.pdf".encode_utf16().collect::<Vec<u16>>()).unwrap();
        let mut r = window(&data, &dir, "report-final-v2.pdf", &base);
        r.h.key("Del");
        r.until(&format!("{var}: the bar"), |r| bar::bar_of(r.h.screen()).is_open());
        assert_eq!(bar::bar_of(r.h.screen()).permanent(), [item].as_slice(), "{var}");
        r.h.key("Esc");
        assert!(r.shared.ops.borrow().rows().is_empty(), "{var}: Esc sent nothing");
    }
    // A local NTFS file inside the test root: recycled, then undone.
    let root = TempRoot::new("disp_del_local");
    let item = WidePath::parse(&root.file("notes.txt", b"made-up bytes").to_string_lossy()).unwrap();
    // The host's fence is this root and this user's folder of the volume's Recycle Bin, where a restore takes the
    // item from: the job's own FILES_TEST_ROOT names only its throwaway volumes and share, and refuses one in %TEMP%.
    let vol = match root.path().components().next() {
        Some(std::path::Component::Prefix(p)) => p.as_os_str().to_string_lossy().into_owned(),
        _ => panic!("{} has no drive", root.path().display()),
    };
    let sid = oled_sys::pipe::current_user_sid().expect("the user's SID");
    let fence = format!("{};{vol}\\$Recycle.Bin\\{sid}", root.path().display());
    let mut r = window(&data, &item.parent().unwrap(), "notes.txt", &fence);
    r.h.key("Del");
    r.until("the classification", |r| !bar::bar_of(r.h.screen()).is_pending());
    if bar::bar_of(r.h.screen()).is_open() {
        // The runner's system volume carries no per-volume Recycle Bin values: Unknown, and the bar asks (U3).
        eprintln!("disposable: the system volume's Recycle Bin facts are unknown here; the bar asked");
        r.h.key("Esc");
        return;
    }
    let path = std::path::PathBuf::from(item.to_string_lossy());
    r.until("the recycle", |r| r.shared.ops.borrow().running() == 0 && !path.exists());
    r.until("the undo record", |r| r.shared.undo.borrow().len() == 1);
    let bin = {
        let mut undo = r.shared.undo.borrow_mut();
        let e = undo.pop().expect("the undo entry");
        let bin = e.record.entries.first().and_then(|x| x.bin.as_ref()).map(|b| b.to_string_lossy());
        undo.push(e);
        bin
    };
    r.h.key("Ctrl+Z");
    if RESTORE_FROM_BIN && !files_engine::gate::REAL_DATA_MUTATIONS {
        // The real-data gate is off in this build (gate.rs): the window changes files only inside the test and live
        // roots, and the item now lies in the Recycle Bin, outside them. The undo is refused before the helper, with
        // the gate's notice; its entry stays, and so does the item, in the bin. The restore needs the gate on.
        eprintln!("disposable: the real-data gate is off: the undo from the bin is refused and the item stays there");
        r.h.render();
        assert!(r.h.a11y_dump().contains(&*t(k::FILES_OPS_PREVIEW_GATE, UiLang::EN)), "the gate's notice");
        assert_eq!(r.shared.undo.borrow().len(), 1, "the refused undo keeps its entry");
        let bin = std::path::PathBuf::from(bin.expect("the item went to the bin").to_string());
        assert!(!path.exists() && bin.exists(), "the item stays in the bin");
    } else if RESTORE_FROM_BIN {
        r.until("the restore", |_| path.exists());
    } else {
        // Open Recycle Bin: the hand-off (its notice until FD-INST); the entry is consumed.
        assert_eq!(r.shared.undo.borrow().len(), 0);
        assert!(!path.exists());
    }
    r.shared.services.ops.shutdown();
}
