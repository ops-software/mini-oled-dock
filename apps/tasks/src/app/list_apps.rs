//! --list-apps (preview builds): the Apps groups with their natural names, in the format of the dock's --list-apps. The
//! live tester compares its output with the dock's. No mutex, no window; config.json's renames are not applied here, while the
//! dock's --list-apps applies them (step "config"): compare on a data root without renames.

use std::collections::HashMap;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use oled_shellmodel::names::{natural_name, NameQuery};
use oled_shellmodel::startmenu::{IndexedSources, StartMenuIndex};
use oled_sysmon::ProcKey;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;

use super::resolver::{list_apps_lines, resolve_identity, system_root};
use crate::engine::cadence::{Control, Refresh};
use crate::engine::frame::Frame;
use crate::engine::host::{default_samplers, FrameSink, SamplerHost};
use crate::engine::need::{Need, NeedSet};
use crate::model::classify::{ImagePath, InfoBook};
use crate::model::group::{group, AppKey, AppWindow, GroupInput, Grouping, Interner};
use crate::model::view::Section;

/// The first frame is waited for this long.
const FRAME_WAIT: Duration = Duration::from_secs(3);

/// The sampler's wake-up: a channel send.
struct ChannelSink(mpsc::Sender<()>);

impl FrameSink for ChannelSink {
    fn ready(&self) {
        let _ = self.0.send(());
    }
}

/// One group of the Apps section: its identity key and AUMID, and the exe of its first window's process.
struct AppRow {
    key: String,
    aumid: Option<String>,
    exe: String,
}

/// T3.4. Exit 0, or 1 without a frame.
pub fn run() -> ExitCode {
    // SAFETY: COM STA for the main thread, paired with CoUninitialize below.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let code = match first_frame() {
        Some(frame) => {
            let rows = apps_of(&frame);
            let index = StartMenuIndex::build();
            let root = system_root();
            let src = IndexedSources { index: &index, system_root: &root };
            let mut lines = Vec::new();
            for r in &rows {
                let q = NameQuery { key: &r.key, exe_path: &r.exe, aumid: r.aumid.as_deref() };
                let (name, step) = natural_name(&q, &src);
                lines.extend(list_apps_lines(&name, step.as_str(), &r.key));
            }
            print_lines(&lines);
            ExitCode::SUCCESS
        }
        None => ExitCode::from(1),
    };
    if com {
        // SAFETY: paired with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    code
}

/// A SamplerHost with a channel sink, Control visible with Procs and Windows: the first frame whose windows have their
/// images (or the last one at the deadline); None when no frame came in 3 s.
fn first_frame() -> Option<Frame> {
    let (tx, rx) = mpsc::channel();
    let need = NeedSet::EMPTY.with(Need::Procs).with(Need::Windows);
    let control = Control {
        visible: true,
        foreground: true,
        minimised_or_cloaked: false,
        display_off: false,
        refresh: Refresh::Half,
        idle: false,
        need,
    };
    let host = SamplerHost::start(Box::new(ChannelSink(tx)), default_samplers(), control).ok()?;
    let deadline = Instant::now() + FRAME_WAIT;
    let mut frame = Frame::default();
    let mut got = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if rx.recv_timeout(left).is_err() {
            break;
        }
        if host.take(&mut frame) {
            got = true;
            if frame.windows.iter().all(|w| frame.paths.get(w.owner).is_some()) {
                break;
            }
        }
    }
    host.stop();
    got.then_some(frame)
}

/// The Apps groups in Apps order, each with its identity as the resolver's functions give it (called directly).
fn apps_of(frame: &Frame) -> Vec<AppRow> {
    let mut names = Interner::new();
    let mut book = InfoBook::new();
    let root = system_root();
    let info = book.update(&frame.procs, &frame.paths, &root, &mut names).to_vec();
    let mut windows = Vec::new();
    let mut rows: HashMap<u32, AppRow> = HashMap::new();
    for w in &frame.windows {
        let exe = match frame.paths.get(w.owner) {
            Some(ImagePath::Path(p)) => p.to_string(),
            _ => String::new(),
        };
        let (key, aumid) = resolve_identity(w.hwnd, w.owner.pid, &exe);
        let id = names.intern(&key);
        rows.entry(id).or_insert(AppRow { key, aumid, exe });
        windows.push(AppWindow { owner: w.owner, identity: crate::model::group::IdentityId(id), hung: w.hung });
    }
    let hosts: [ProcKey; 0] = [];
    let mut session = 0u32;
    // SAFETY: `session` is a valid local u32 for writing.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    let input = GroupInput { procs: &frame.procs, info: &info, windows: &windows, session, hosts: &hosts };
    let mut grouping = Grouping::default();
    group(&input, &mut grouping);
    grouping
        .groups
        .iter()
        .filter(|g| g.section == Section::Apps)
        .filter_map(|g| match g.key {
            AppKey::App(id) => rows.remove(&id.0),
            _ => None,
        })
        .collect()
}

/// Lines on the console of the parent, when it has one.
fn print_lines(lines: &[String]) {
    // SAFETY: no pointers; fails harmlessly when the parent has no console.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    let mut out = std::io::stdout().lock();
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
}
