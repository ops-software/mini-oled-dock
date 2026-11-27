//! The suite manifest (suite.json): components, hashes and the convergence plan (F-P6).
// PF1 freeze: ComponentId, SUITE_FILES, LANG_DIR and the constants are real (P-FC ships them in 1.7.0); the
// functions are stubs until U1.
#![allow(dead_code, unused_variables)]

use super::release::Version;
pub const SUITE_ASSET: &str = "suite.json";
pub const SUITE_MAX_BYTES: u64 = 64 * 1024;
pub const SUITE_SCHEMA: u16 = 1;
pub const COMPONENT_MAX_BYTES: u64 = 32 * 1024 * 1024;
pub const SUITE_FILES: [&str; 2] = ["suite.json", "suite.json.tmp"]; // install-relative files of the suite itself
pub const LANG_DIR: &str = "lang";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ComponentId {
    Dock,
    Tasks,
    TasksAdmin,
    Files,
    LangIt,
}
impl ComponentId {
    // implemented in PF1 (P-FC ships it in 1.7.0)
    pub const ALL: [ComponentId; 5] =
        [ComponentId::Dock, ComponentId::Tasks, ComponentId::TasksAdmin, ComponentId::Files, ComponentId::LangIt]; // in this order; the manifest lists components in it
    pub fn id(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock",
            ComponentId::Tasks => "tasks",
            ComponentId::TasksAdmin => "tasks-admin",
            ComponentId::Files => "files",
            ComponentId::LangIt => "lang-it",
        }
    }
    pub fn file(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.exe",
            ComponentId::Tasks => "tasks.exe",
            ComponentId::TasksAdmin => "tasks-admin.exe",
            ComponentId::Files => "files.exe",
            ComponentId::LangIt => "lang\\it.lang",
        }
    }
    pub fn prev(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.prev.exe",
            ComponentId::Tasks => "tasks.prev.exe",
            ComponentId::TasksAdmin => "tasks-admin.prev.exe",
            ComponentId::Files => "files.prev.exe",
            ComponentId::LangIt => "lang\\it.prev.lang",
        }
    }
    pub fn asset(self) -> &'static str {
        match self {
            ComponentId::Dock => "dock.exe",
            ComponentId::Tasks => "tasks.exe",
            ComponentId::TasksAdmin => "tasks-admin.exe",
            ComponentId::Files => "files.exe",
            ComponentId::LangIt => "lang-it.lang",
        }
    }
    pub fn install_names(self) -> [String; 4] {
        let (file, prev) = (self.file(), self.prev());
        [file.to_string(), format!("{file}.new"), prev.to_string(), format!("{prev}.new")]
    }
    pub fn parse(s: &str) -> Option<ComponentId> {
        ComponentId::ALL.into_iter().find(|c| c.id() == s)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Beta,
    Stable,
} // ROADMAP X34; "beta" | "stable"
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub id: ComponentId,
    pub version: Version,
    pub sha256: [u8; 32],
    pub size: u64,
    pub stage: Stage,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteManifest {
    pub schema: u16,
    pub version: Version,
    pub components: Vec<Component>,
}
pub fn parse_manifest(bytes: &[u8]) -> Result<SuiteManifest, String> {
    unimplemented!("PF1 stub")
}
pub fn encode_manifest(m: &SuiteManifest) -> String {
    unimplemented!("PF1 stub")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnDisk {
    pub id: ComponentId,
    pub current: Option<[u8; 32]>,
    pub prev: Option<[u8; 32]>,
    pub staged: Option<[u8; 32]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Stage,
    Prev,
    Download,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep(ComponentId),
    Add(ComponentId, Source),
    Replace(ComponentId, Source),
    Skip(ComponentId),
}
pub fn plan_convergence(
    own: &SuiteManifest,
    disk: &[OnDisk],
    bad: &[(ComponentId, [u8; 32])],
    network: bool,
) -> Vec<Step> {
    unimplemented!("PF1 stub")
}
//   never a step for Dock (the guard owns dock.exe); Keep when current == own hash; else Prev if prev == own hash, else Stage if
//   staged == own hash, else Download if network, else Skip; Skip also for a bad (id, hash) with three strikes; a component on
//   disk but absent from own is Keep
