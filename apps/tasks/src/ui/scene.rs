//! Scenes: apps/tasks/scenes/*.json (fixture frames and names) loaded by the Harness tests from disk and embedded in
//! the preview build; they enter the screen only through InjectFrame and Deps::inert. Filled by TA-T1a (T1.6).

use std::collections::BTreeMap;
use std::sync::Arc;

use oled_sysmon::cpu::CpuDelta;
use oled_sysmon::mem::MemSample;
use oled_sysmon::percore::CoreTimes;
use oled_sysmon::{ProcKey, ProcRow, ProcTable, SysmonError};
use oled_ui::harness::Harness;
use oled_ui::{Lang, PointL, SizeL, Tokens, UiLang};
use serde::Deserialize;

use super::grouped::HostEnv;
use super::screen::{InjectFrame, TasksScreen};
use crate::app::args::PreviewLang;
use crate::app::exec::Executor;
use crate::app::launch::Launcher;
use crate::app::resolver::{on_done, Answer, ResolverDone};
use crate::app::Deps;
use crate::engine::frame::{Frame, ServiceEntry, ServiceMap, WindowFact};
use crate::model::classify::{ImagePath, PathMap};
use crate::model::group::ExeId;
use crate::model::view::{ColumnId, Section, ViewMode};

/// A scene file may not be larger than this.
pub const FILE_CAP: usize = 256 * 1024;
/// The sampler's period the scene's ticks stand for.
const TICK_MS: u32 = 1000;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SceneMode {
    #[default]
    List,
    Grouped,
    Tree,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SceneCol {
    Name,
    Cpu,
    Memory,
    Io,
    Pid,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SceneSort {
    pub col: SceneCol,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneMachine {
    pub logical_cpus: u32,
    pub total_phys: u64,
    pub mem_load: u8,
    #[serde(default)]
    pub commit_pct: u8,
    pub cpu_pct: f32,
    #[serde(default)]
    pub dpc_irq_pct: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SceneProc {
    pub pid: u32,
    pub created: u64,
    #[serde(default)]
    pub parent: u32,
    #[serde(default = "one")]
    pub session: u32,
    pub image: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Whole-processor percent per tick; the last value repeats.
    #[serde(default)]
    pub cpu: Vec<f32>,
    pub ws_private: u64,
    /// Bytes per second per tick; the last value repeats.
    #[serde(default)]
    pub io: Vec<f32>,
    #[serde(default)]
    pub threads: u32,
    #[serde(default)]
    pub handles: u32,
}

fn one() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SceneWindow {
    pub pid: u32,
    pub identity: String,
    #[serde(default)]
    pub hung: bool,
}

/// A service a process hosts (the names of the service list).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SceneService {
    pub pid: u32,
    pub name: String,
    pub display: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub size: [i32; 2],
    #[serde(default)]
    pub mode: SceneMode,
    #[serde(default)]
    pub sort: Option<SceneSort>,
    pub machine: SceneMachine,
    pub procs: Vec<SceneProc>,
    #[serde(default)]
    pub windows: Vec<SceneWindow>,
    #[serde(default)]
    pub services: Vec<SceneService>,
    /// Sections that start open or closed (by name: apps, background, windows); true is collapsed.
    #[serde(default)]
    pub collapsed: BTreeMap<String, bool>,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// Frames after the first one: 0 is a single unprimed frame.
    #[serde(default)]
    pub ticks: u32,
    #[serde(default)]
    pub held: bool,
    /// A failed snapshot after the frames: the Win32 code.
    #[serde(default)]
    pub read_error: Option<u32>,
    #[serde(default)]
    pub input: Vec<String>,
}

impl Scene {
    /// Parses a scene's text; unknown fields and a text over FILE_CAP are refused.
    pub fn parse(text: &str) -> Result<Scene, String> {
        if text.len() > FILE_CAP {
            return Err(format!("scene is {} bytes, the cap is {FILE_CAP}", text.len()));
        }
        serde_json::from_str(text).map_err(|e| format!("scene: {e}"))
    }

    /// Reads a scene file.
    pub fn load(path: &std::path::Path) -> Result<Scene, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > FILE_CAP as u64 {
            return Err(format!("{}: larger than {FILE_CAP} bytes", path.display()));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Scene::parse(&text)
    }

    /// Ticks + 1 frames whose counters advance so the engine's rates come out as the scene says at dt 1000 ms, then the
    /// failed snapshot when the scene has one.
    pub fn frames(&self) -> Vec<Frame> {
        let at = |v: &[f32], j: u32| v.get(j as usize).or_else(|| v.last()).copied().unwrap_or(0.0);
        let cpus = self.machine.logical_cpus.max(1);
        let mut out = Vec::new();
        // The counters of every process, summed tick by tick.
        let mut counters = vec![(0.0f64, 0.0f64); self.procs.len()];
        for i in 0..=self.ticks {
            let mut procs = ProcTable::new();
            let mut paths = Vec::new();
            for (p, (cpu, io)) in self.procs.iter().zip(counters.iter_mut()) {
                if i > 0 {
                    *cpu += f64::from(at(&p.cpu, i - 1)) / 100.0 * f64::from(TICK_MS) * 10_000.0 * f64::from(cpus);
                    *io += f64::from(at(&p.io, i - 1)) * f64::from(TICK_MS) / 1000.0;
                }
                let (cpu, io) = (*cpu, *io);
                let utf16: Vec<u16> = p.image.encode_utf16().collect();
                let image = procs.names.intern(&utf16);
                let key = ProcKey { pid: p.pid, created: p.created };
                procs.push(ProcRow {
                    key,
                    parent_pid: p.parent,
                    session: p.session,
                    image,
                    cpu_100ns: cpu as u64,
                    ws: p.ws_private,
                    ws_peak: p.ws_private,
                    ws_private: p.ws_private,
                    private_bytes: p.ws_private,
                    io_read: io as u64,
                    threads: p.threads,
                    handles: p.handles,
                    ..ProcRow::default()
                });
                paths.push((key, p.path.as_deref().map_or(ImagePath::Unknown, |s| ImagePath::Path(Arc::from(s)))));
            }
            procs.finish();
            paths.sort_by_key(|e| e.0);
            let m = &self.machine;
            let mut f = Frame {
                seq: u64::from(i) + 1,
                t_us: (u64::from(i) + 1) * u64::from(TICK_MS) * 1000,
                dt_ms: if i == 0 { 0 } else { TICK_MS },
                procs,
                paths: Arc::new(PathMap { entries: paths }),
                ..Frame::default()
            };
            f.sys.logical_cpus = cpus;
            // One core whose interrupt and driver time is the scene's share of its busy time, tick by tick.
            let busy = u64::from(i) * u64::from(TICK_MS) * 10_000;
            f.sys.cores = vec![CoreTimes {
                idle: 0,
                kernel: busy,
                user: 0,
                dpc: (busy as f64 * f64::from(m.dpc_irq_pct) / 100.0) as u64,
                interrupt: 0,
            }];
            f.sys.cpu = Some(CpuDelta { d_idle: (1000.0 - m.cpu_pct * 10.0).max(0.0) as u64, d_total: 1000 });
            f.sys.mem = Some(MemSample {
                load_pct: m.mem_load,
                total_phys: m.total_phys,
                avail_phys: m.total_phys / 100 * u64::from(100 - m.mem_load.min(100)),
                commit_total: u64::from(m.commit_pct),
                commit_limit: 100,
            });
            let mut services: Vec<ServiceEntry> = self
                .services
                .iter()
                .map(|s| ServiceEntry { pid: s.pid, name: s.name.as_str().into(), display: s.display.as_str().into() })
                .collect();
            services.sort_by(|a, b| (a.pid, &a.name).cmp(&(b.pid, &b.name)));
            f.services = Arc::new(ServiceMap { entries: services });
            f.services_fresh = true;
            f.windows = self
                .windows
                .iter()
                .filter_map(|w| {
                    let row = self.procs.iter().find(|p| p.pid == w.pid)?;
                    Some(WindowFact {
                        owner: ProcKey { pid: row.pid, created: row.created },
                        hwnd: 0x1000 + w.pid as isize,
                        hung: w.hung,
                    })
                })
                .collect();
            out.push(f);
        }
        if let Some(code) = self.read_error {
            let t_us = (u64::from(self.ticks) + 2) * u64::from(TICK_MS) * 1000;
            out.push(Frame {
                seq: u64::from(self.ticks) + 2,
                t_us,
                dt_ms: TICK_MS,
                error: Some(SysmonError::Status(code as i32)),
                ..Frame::default()
            });
        }
        out
    }
}

impl From<SceneCol> for ColumnId {
    fn from(c: SceneCol) -> ColumnId {
        match c {
            SceneCol::Name => ColumnId::Name,
            SceneCol::Cpu => ColumnId::Cpu,
            SceneCol::Memory => ColumnId::Memory,
            SceneCol::Io => ColumnId::Io,
            SceneCol::Pid => ColumnId::Pid,
        }
    }
}

/// The scene in a headless window at `dpi` and `lang`: frames sent through InjectFrame over Deps::inert, then the
/// input replayed. The caller keeps its own handles on the recorders to read them.
pub fn open(
    scene: &Scene,
    lang: PreviewLang,
    dpi: u32,
    launcher: Arc<dyn Launcher>,
    exec: Arc<dyn Executor>,
) -> Result<Harness<TasksScreen>, String> {
    open_with(scene, lang, dpi, Tokens::oled(), launcher, exec)
}

/// open() in a given theme (the goldens' contrast and high-contrast cells).
pub fn open_with(
    scene: &Scene,
    lang: PreviewLang,
    dpi: u32,
    tokens: Tokens,
    launcher: Arc<dyn Launcher>,
    exec: Arc<dyn Executor>,
) -> Result<Harness<TasksScreen>, String> {
    let ui = UiLang { italian: lang == PreviewLang::It, pseudo: lang == PreviewLang::Pseudo };
    let deps = Deps::inert(ui, launcher, exec);
    let size = SizeL { w: scene.size[0], h: scene.size[1] };
    if size.w < 200 || size.h < 200 || size.w > 4000 || size.h > 4000 {
        return Err(format!("scene size {}x{} is out of range", size.w, size.h));
    }
    let mut h = Harness::new(TasksScreen::new(deps), size, dpi, tokens)
        .lang(if lang == PreviewLang::It { Lang::It } else { Lang::En })
        .pseudo(lang == PreviewLang::Pseudo);
    let names: Vec<(String, String)> = scene.names.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    h.screen_mut().p.names.set_fixture_names(&names);
    // The windows' identities as the resolver would have answered them, and the programs' names by exe key.
    let mut answers = Vec::new();
    for w in &scene.windows {
        answers.push(Answer::Identity {
            hwnd: 0x1000 + w.pid as isize,
            pid: w.pid,
            key: w.identity.clone(),
            aumid: None,
        });
    }
    for p in &scene.procs {
        let Some(path) = p.path.as_deref() else { continue };
        if let Some(name) = scene.names.get(&oled_shellmodel::identity::key_from_exe_path(path)) {
            let exe = ExeId(h.screen().v.intern(&path.to_ascii_lowercase()));
            answers.push(Answer::Name { identity: None, exe_id: Some(exe), name: Some(name.clone()) });
        }
    }
    on_done(&mut h.screen_mut().p.names, ResolverDone(answers));
    for (name, on) in &scene.collapsed {
        let section = match name.as_str() {
            "apps" => Section::Apps,
            "background" => Section::Background,
            "windows" => Section::Windows,
            other => return Err(format!("scene: unknown section {other:?}")),
        };
        h.screen_mut().deps.state.set_collapsed(section, *on);
    }
    // The shell is the scene's explorer; no process of the scene is this one; the scene's user is session 1.
    let shell_pid = scene.procs.iter().find(|p| p.image.eq_ignore_ascii_case("explorer.exe")).map(|p| p.pid);
    let env = HostEnv { shell_pid, own_pid: u32::MAX, session: 1 };
    h.screen_mut().p.grouped.set_env(env);
    // The actions' guard is built against the same environment, and the system is never probed for the scene's pids.
    h.screen_mut().p.actions.set_env(env);
    h.screen_mut().v.set_mode(match scene.mode {
        SceneMode::List => ViewMode::List,
        SceneMode::Grouped => ViewMode::Grouped,
        SceneMode::Tree => ViewMode::Tree,
    });
    // The More menu's cost line: the figures of a quiet process, so the picture is the same on every machine.
    h.screen_mut().v.set_fixture_cost(0.3, 38 << 20);
    if let Some(s) = scene.sort {
        h.screen_mut().v.pin_sort(s.col.into(), s.desc);
    }
    for f in scene.frames() {
        h.send(Box::new(InjectFrame(f)));
    }
    for line in &scene.input {
        replay(&mut h, line)?;
    }
    Ok(h)
}

fn point(s: &str) -> Result<PointL, String> {
    let (x, y) = s.split_once(',').ok_or_else(|| format!("input: {s:?} is not x,y"))?;
    let n = |v: &str| v.trim().parse::<i32>().map_err(|_| format!("input: {v:?} is not a number"));
    Ok(PointL { x: n(x)?, y: n(y)? })
}

/// One line of a scene's input.
fn replay(h: &mut Harness<TasksScreen>, line: &str) -> Result<(), String> {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "hover" => {
            h.hover(point(rest)?);
        }
        "click" => {
            h.click(point(rest)?);
        }
        "double_click" => {
            h.double_click(point(rest)?);
        }
        "right_click" => {
            h.right_click(point(rest)?);
        }
        "wheel" => {
            let (at, n) = rest.rsplit_once(' ').ok_or_else(|| format!("input: {line:?}"))?;
            let n: f32 = n.parse().map_err(|_| format!("input: {line:?}"))?;
            h.wheel(point(at)?, n);
        }
        "key" => {
            if oled_ui::Chord::parse(rest).is_none() {
                return Err(format!("input: {rest:?} is not a chord"));
            }
            h.key(rest);
        }
        "type" => {
            h.type_text(rest);
        }
        "advance" => {
            h.advance(rest.trim().parse().map_err(|_| format!("input: {line:?}"))?);
        }
        // A routed Updated message that finds a newer file on disk (TS3), and a command as a menu item would send it.
        "updated" => {
            h.screen_mut().p.updated.set_fixture_newer();
            h.app_message(crate::app::updated::updated_message(), 0, 0);
        }
        "command" => {
            let id = match rest.trim() {
                "about" => crate::ui::commands::ABOUT,
                "restart" => crate::ui::commands::RESTART_TO_UPDATE,
                other => return Err(format!("input: unknown command {other:?}")),
            };
            h.send(Box::new(super::screen::InjectCommand(id)));
        }
        "active" => {
            h.set_active(rest.trim() == "on");
        }
        _ => return Err(format!("input: unknown verb {verb:?}")),
    }
    Ok(())
}
