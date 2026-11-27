//! The seams between the window and the engine (P3-files-app 2.2): one trait per engine service, the Services bundle a
//! window holds, and the real adapters. Every engine client is built lazily, on the adapter's first call that needs
//! it, so real() starts no thread or process and the engine's bodies are reached only when the window uses them.
//! real() is called only on the start path (app.rs): it is the one place that resolves the window's own folders.

use std::cell::{Cell, OnceCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use oled_trust::frame::files::{CallKind, ItemRef, OpId, ReqId, ShellReq};

use crate::bg::{Bg, JobFn, Lane};
use crate::decor::cache::{ClassIconSource, ClassIcons, IconRef, CLASS_ICONS_IN_UI};
use crate::format::LocalTime;
use crate::host::TestHooks;
use crate::hub::Hub;
use crate::listing::{EntryStore, EnumJob, EnumPool, Gen, TabId, WidePath};
use crate::model::Listing;
use crate::ops_client::{OpsClient, OpsConfig, OpsError, OpsEvent, UndoRecord};
use crate::options::{self, FilesOptions};
use crate::own_files::OwnDir;
use crate::shell_client::{HostState, ShellClient, ShellConfig, ShellReply};
use crate::watch::{WatchError, WatchEvent, WatchId, Watcher};

/// Listings and the enumeration pool. Real: EntryStore::new; fake: fakes::FakeListing.
pub trait EnumService {
    fn submit(&self, job: EnumJob);
    fn new_listing(&self, dir: WidePath, gen: Gen) -> Box<dyn Listing>;
}

/// Every change of user files: the ops host through IFileOperation. Only ops_ui::request calls the mutating methods.
pub trait OpsService {
    fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError>;
    fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError>;
    fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError>;
    fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError>;
    fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError>;
    fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError>;
    fn cancel(&self, op: OpId);
    fn pause(&self, op: OpId, on: bool);
    fn bring_forward(&self, op: OpId);
    fn ack_done(&self, of: OpId);
    fn host_pid(&self) -> Option<u32>;
    fn drain(&self) -> Vec<OpsEvent>;
    fn shutdown(&self);
}

/// The shell host: menus, verbs, icons, type names, pins.
pub trait ShellService {
    fn next_req(&self) -> ReqId;
    fn request(&self, r: ShellReq);
    fn drain(&self) -> Vec<ShellReply>;
    fn state(&self) -> HostState;
    fn prespawn(&self);
    fn host_pid(&self) -> Option<u32>;
    fn disabled(&self, kind: CallKind, ext: &str) -> bool;
    fn shutdown(&self);
}

pub trait WatchService {
    fn watch(&self, dir: &WidePath, gen: Gen) -> Result<WatchId, WatchError>;
    fn unwatch(&self, id: WatchId);
    fn drives(&self, on: bool);
    fn poke_devices(&self);
    fn drain(&self) -> Vec<WatchEvent>;
}

/// ClassIcons' five methods (P1-files-engine 2.6).
pub trait IconService {
    fn get(&self, ext: &[u8], px: u16) -> IconRef;
    fn note(&self, ext: &[u8]);
    fn on_shell_reply(&self, r: &ShellReply) -> bool;
    fn assoc_changed(&self);
    fn rebuild_if_due(&self);
}

pub trait BgService {
    fn run(&self, win: u32, lane: Lane, job: JobFn);
}

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn local_now(&self) -> LocalTime;
}

/// The window's own files: session.json, types.json, files-guard.json.
pub trait OwnFiles {
    /// None: absent, unreadable or over the cap.
    fn read(&self, rel: &str, cap: usize) -> Option<Vec<u8>>;
    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> bool;
}

/// files.json.
pub trait OptionsService {
    fn get(&self) -> FilesOptions;
    /// True when kept (saved by the real one once FA-OPT lands; recorded by the fake).
    fn modify(&self, f: &mut dyn FnMut(&mut FilesOptions)) -> bool;
}

pub struct Services {
    pub enumr: Rc<dyn EnumService>,
    pub ops: Rc<dyn OpsService>,
    pub shell: Rc<dyn ShellService>,
    pub watch: Rc<dyn WatchService>,
    pub icons: Rc<dyn IconService>,
    pub bg: Rc<dyn BgService>,
    pub clock: Rc<dyn Clock>,
    pub own: Rc<dyn OwnFiles>,
    pub options: Rc<dyn OptionsService>,
}

/// Who asked the shell host for something: the reply goes back to that window (or to the process-wide owner).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReqOwner {
    /// gen: the tab's icon-request counter (F2.2).
    Icons {
        win: u32,
        tab: TabId,
        gen: Gen,
    },
    TypeNames,
    Menu {
        win: u32,
    },
    Exec {
        win: u32,
    },
    Pins,
    ShellNew,
}

/// The real services. Called only by app.rs: it resolves the own data folder (FILES_DATA_DIR in a debug child) and the
/// options; no engine client is built here.
pub fn real(hub: &Arc<Hub>, exe: PathBuf, test: Option<TestHooks>) -> Services {
    let data = OwnDir::default_dir();
    let data_dir = data.as_ref().map(|d| d.path().to_path_buf()).unwrap_or_default();
    let shell = real_shell_impl(hub, exe.clone(), test.clone());
    Services {
        enumr: Rc::new(RealEnum { sink: hub.sink(), pool: OnceCell::new() }),
        ops: Rc::new(RealOps {
            cfg: Cell::new(Some(OpsConfig { exe, data_dir: data_dir.clone(), owner: 0, test })),
            notify: hub.notifier(),
            client: OnceCell::new(),
        }),
        icons: Rc::new(RealIcons {
            shell: Rc::clone(&shell),
            data_dir,
            notify: hub.notifier(),
            icons: OnceCell::new(),
        }),
        shell,
        watch: Rc::new(RealWatch { notify: hub.notifier(), watcher: OnceCell::new() }),
        bg: Rc::new(RealBg(Bg::new(Arc::clone(hub)))),
        clock: Rc::new(RealClock(Instant::now())),
        own: Rc::new(RealOwn { dir: data, warned: Cell::new(false) }),
        options: options::real_service(),
    }
}

/// The lazy ShellClient adapter real() uses; it resolves no folder, so the debug-host UI tests may call it.
pub fn real_shell(hub: &Arc<Hub>, exe: PathBuf, test: Option<TestHooks>) -> Rc<dyn ShellService> {
    real_shell_impl(hub, exe, test)
}

fn real_shell_impl(hub: &Arc<Hub>, exe: PathBuf, test: Option<TestHooks>) -> Rc<RealShell> {
    Rc::new(RealShell {
        cfg: Cell::new(Some(ShellConfig { exe, test })),
        notify: hub.notifier(),
        client: OnceCell::new(),
    })
}

struct RealEnum {
    sink: crate::listing::BatchSink,
    pool: OnceCell<EnumPool>,
}

impl EnumService for RealEnum {
    fn submit(&self, job: EnumJob) {
        self.pool.get_or_init(|| EnumPool::new(Arc::clone(&self.sink))).submit(job);
    }

    fn new_listing(&self, dir: WidePath, gen: Gen) -> Box<dyn Listing> {
        Box::new(EntryStore::new(dir, gen))
    }
}

struct RealOps {
    cfg: Cell<Option<OpsConfig>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    client: OnceCell<OpsClient>,
}

impl RealOps {
    /// The client, built on the first call that needs it (the first mutation).
    fn client(&self) -> &OpsClient {
        self.client.get_or_init(|| {
            let cfg = self.cfg.take().unwrap_or(OpsConfig {
                exe: PathBuf::new(),
                data_dir: PathBuf::new(),
                owner: 0,
                test: None,
            });
            OpsClient::new(cfg, Arc::clone(&self.notify))
        })
    }
}

impl OpsService for RealOps {
    fn recycle(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.client().recycle(items)
    }
    fn delete_permanent(&self, items: &[ItemRef]) -> Result<OpId, OpsError> {
        self.client().delete_permanent(items)
    }
    fn rename(&self, item: &WidePath, to: &str) -> Result<OpId, OpsError> {
        self.client().rename(item, to)
    }
    fn new_folder(&self, parent: &WidePath, name: &str) -> Result<OpId, OpsError> {
        self.client().new_folder(parent, name)
    }
    fn undo(&self, rec: &UndoRecord) -> Result<OpId, OpsError> {
        self.client().undo(rec)
    }
    fn resume_rest(&self, of: OpId) -> Result<OpId, OpsError> {
        self.client().resume_rest(of)
    }
    fn cancel(&self, op: OpId) {
        if let Some(c) = self.client.get() {
            c.cancel(op);
        }
    }
    fn pause(&self, op: OpId, on: bool) {
        if let Some(c) = self.client.get() {
            c.pause(op, on);
        }
    }
    fn bring_forward(&self, op: OpId) {
        if let Some(c) = self.client.get() {
            c.bring_forward(op);
        }
    }
    fn ack_done(&self, of: OpId) {
        if let Some(c) = self.client.get() {
            c.ack_done(of);
        }
    }
    fn host_pid(&self) -> Option<u32> {
        self.client.get().and_then(OpsClient::host_pid)
    }
    fn drain(&self) -> Vec<OpsEvent> {
        self.client.get().map(OpsClient::drain).unwrap_or_default()
    }
    fn shutdown(&self) {
        if let Some(c) = self.client.get() {
            c.shutdown();
        }
    }
}

struct RealShell {
    cfg: Cell<Option<ShellConfig>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    client: OnceCell<Arc<ShellClient>>,
}

impl RealShell {
    fn client(&self) -> &Arc<ShellClient> {
        self.client.get_or_init(|| {
            let cfg = self.cfg.take().unwrap_or(ShellConfig { exe: PathBuf::new(), test: None });
            Arc::new(ShellClient::new(cfg, Arc::clone(&self.notify)))
        })
    }
}

impl ShellService for RealShell {
    fn next_req(&self) -> ReqId {
        self.client().next_req()
    }
    fn request(&self, r: ShellReq) {
        self.client().request(r);
    }
    fn drain(&self) -> Vec<ShellReply> {
        self.client.get().map(|c| c.drain()).unwrap_or_default()
    }
    fn state(&self) -> HostState {
        self.client.get().map_or(HostState::Down, |c| c.state())
    }
    fn prespawn(&self) {
        self.client().prespawn();
    }
    fn host_pid(&self) -> Option<u32> {
        self.client.get().and_then(|c| c.host_pid())
    }
    fn disabled(&self, kind: CallKind, ext: &str) -> bool {
        self.client.get().is_some_and(|c| c.disabled(kind, ext))
    }
    fn shutdown(&self) {
        if let Some(c) = self.client.get() {
            c.shutdown();
        }
    }
}

struct RealWatch {
    notify: Arc<dyn Fn() + Send + Sync>,
    watcher: OnceCell<Watcher>,
}

impl RealWatch {
    fn watcher(&self) -> &Watcher {
        self.watcher.get_or_init(|| Watcher::new(Arc::clone(&self.notify)))
    }
}

impl WatchService for RealWatch {
    fn watch(&self, dir: &WidePath, gen: Gen) -> Result<WatchId, WatchError> {
        self.watcher().watch(dir, gen)
    }
    fn unwatch(&self, id: WatchId) {
        if let Some(w) = self.watcher.get() {
            w.unwatch(id);
        }
    }
    fn drives(&self, on: bool) {
        if on || self.watcher.get().is_some() {
            self.watcher().drives(on);
        }
    }
    fn poke_devices(&self) {
        if let Some(w) = self.watcher.get() {
            w.poke_devices();
        }
    }
    fn drain(&self) -> Vec<WatchEvent> {
        self.watcher.get().map(Watcher::drain).unwrap_or_default()
    }
}

/// ClassIcons over the shared ShellClient (built together on the first icon asked for).
struct RealIcons {
    shell: Rc<RealShell>,
    data_dir: PathBuf,
    notify: Arc<dyn Fn() + Send + Sync>,
    icons: OnceCell<ClassIcons>,
}

impl RealIcons {
    fn icons(&self) -> &ClassIcons {
        self.icons.get_or_init(|| {
            let source = if CLASS_ICONS_IN_UI { ClassIconSource::InProcess } else { ClassIconSource::ShellHost };
            let shell = Arc::clone(self.shell.client());
            ClassIcons::new(source, self.data_dir.clone(), Some(shell), Arc::clone(&self.notify))
        })
    }
}

impl IconService for RealIcons {
    fn get(&self, ext: &[u8], px: u16) -> IconRef {
        self.icons().get(ext, px)
    }
    fn note(&self, ext: &[u8]) {
        self.icons().note(ext);
    }
    fn on_shell_reply(&self, r: &ShellReply) -> bool {
        self.icons.get().is_some_and(|i| i.on_shell_reply(r))
    }
    fn assoc_changed(&self) {
        if let Some(i) = self.icons.get() {
            i.assoc_changed();
        }
    }
    fn rebuild_if_due(&self) {
        self.icons().rebuild_if_due();
    }
}

struct RealBg(Bg);

impl BgService for RealBg {
    fn run(&self, win: u32, lane: Lane, job: JobFn) {
        self.0.run(win, lane, job);
    }
}

/// Milliseconds since the services were made; the local wall clock from GetLocalTime.
struct RealClock(Instant);

impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn local_now(&self) -> LocalTime {
        // SAFETY: GetLocalTime only writes the returned structure.
        let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        LocalTime {
            year: st.wYear,
            month: u8::try_from(st.wMonth).unwrap_or(1),
            day: u8::try_from(st.wDay).unwrap_or(1),
            hour: u8::try_from(st.wHour).unwrap_or(0),
            minute: u8::try_from(st.wMinute).unwrap_or(0),
        }
    }
}

/// The own data folder through OwnDir; without a folder it keeps nothing and logs that once.
struct RealOwn {
    dir: Option<OwnDir>,
    warned: Cell<bool>,
}

impl RealOwn {
    fn dir(&self) -> Option<&OwnDir> {
        if self.dir.is_none() && !self.warned.replace(true) {
            oled_sys::log!("files: no data folder: session, types and the loop guard are not kept");
        }
        self.dir.as_ref()
    }
}

impl OwnFiles for RealOwn {
    fn read(&self, rel: &str, cap: usize) -> Option<Vec<u8>> {
        let d = self.dir()?;
        // The size is checked before the read, so a huge or corrupt file is never loaded (0.5); the check after it
        // covers a file that grew in between.
        let len = std::fs::symlink_metadata(d.path().join(rel)).ok()?.len();
        if len > cap as u64 {
            oled_sys::log!("files: {rel} is over its {cap} byte cap: not read");
            return None;
        }
        d.read(rel).ok().filter(|b| b.len() <= cap)
    }

    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> bool {
        self.dir().is_some_and(|d| d.write_atomic(rel, bytes).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names;
    use crate::testkit::TempRoot;

    #[test]
    fn own_read_refuses_a_file_over_its_cap() {
        let root = TempRoot::new("own_read_cap");
        let dir = OwnDir::open(&root.dir("own")).expect("open the data folder");
        let own = RealOwn { dir: Some(dir), warned: Cell::new(false) };
        assert!(own.write_atomic(names::SESSION_FILE, &[b'x'; 64]));
        assert_eq!(own.read(names::SESSION_FILE, 64).map(|b| b.len()), Some(64), "at the cap");
        assert_eq!(own.read(names::SESSION_FILE, 63), None, "over the cap");
        assert_eq!(own.read(names::TYPES_FILE, 64), None, "absent");
        assert_eq!(own.read("..\\outside.json", 64), None, "outside the folder");
        let none = RealOwn { dir: None, warned: Cell::new(false) };
        assert_eq!(none.read(names::SESSION_FILE, 64), None, "no folder");
    }
}
