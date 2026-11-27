//! The suite's update path: one manifest, several exes. Lands with U1e; until then it says it is not available.
#![allow(dead_code, unused_variables)]

use oled_trust::manifest::suite::{ComponentId, Stage};

/// `--suite-sync [--network]`: U1e.
pub fn run_sync(network: bool) -> i32 {
    crate::logger::say("--suite-sync: not available in this build");
    2
}

pub struct SuiteStatus {
    pub version: String,
    pub components: Vec<ComponentStatus>,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
}

pub struct ComponentStatus {
    pub id: ComponentId,
    pub installed: bool,
    pub version: Option<String>,
    pub stage: Option<Stage>,
    pub enabled: bool,
}

/// What the Suite page shows: U1e. Until then nothing is installed.
pub fn status() -> SuiteStatus {
    SuiteStatus { version: String::new(), components: Vec::new(), last_sync: None, last_error: None }
}
