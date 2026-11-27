//! session.json (P3-files-app 2.2; FA-WIN, F1a.10): placements, recent network places, known folders and the windows to
//! reopen, through Services.own. The file is read capped, decoded totally and written whole.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::rc::Rc;

use oled_ui::host::{Placement, PlacementKey, PlacementStore};
use oled_ui::RectP;
use serde::{Deserialize, Serialize};

use crate::names;
use crate::services::OwnFiles;
use crate::window::Shared;

/// The file is read up to this many bytes; a larger one is not read.
pub const FILE_CAP: usize = 16 * 1024;
/// Windows and tabs the reopen list keeps (F4.14).
const REOPEN_WINDOWS: usize = 8;
const REOPEN_TABS: usize = 8;
/// Recent network places kept.
const NETWORK_MAX: usize = 16;
/// Placements and known folders kept.
const MAP_MAX: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub placements: BTreeMap<String, Placement>,
    pub network_recent: Vec<(String, i64)>,
    pub known: BTreeMap<String, String>,
    /// At most 8 windows x 8 tabs (F4.14).
    pub reopen: Vec<Vec<String>>,
}

/// One placement as the file holds it.
#[derive(Serialize, Deserialize)]
struct Pl {
    #[serde(default)]
    monitor: String,
    l: i32,
    t: i32,
    r: i32,
    b: i32,
    dpi: u32,
    #[serde(default)]
    max: bool,
}

/// The file's shape; every field may be missing.
#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default)]
    placements: BTreeMap<String, Pl>,
    #[serde(default)]
    network_recent: Vec<(String, i64)>,
    #[serde(default)]
    known: BTreeMap<String, String>,
    #[serde(default)]
    reopen: Vec<Vec<String>>,
}

impl Session {
    /// Read capped at 16 KiB; a malformed file gives defaults and one log line.
    pub fn load(own: &dyn OwnFiles) -> Session {
        let Some(bytes) = own.read(names::SESSION_FILE, FILE_CAP) else { return Session::default() };
        match serde_json::from_slice::<File>(&bytes) {
            Ok(f) => Session::from_file(f),
            Err(e) => {
                oled_sys::log!("files: {} is not usable ({e}): defaults", names::SESSION_FILE);
                Session::default()
            }
        }
    }

    fn from_file(f: File) -> Session {
        let placements = f
            .placements
            .into_iter()
            .filter(|(_, p)| p.r > p.l && p.b > p.t && p.dpi >= 48)
            .take(MAP_MAX)
            .map(|(k, p)| {
                let rect = RectP { l: p.l, t: p.t, r: p.r, b: p.b };
                (k, Placement { monitor: p.monitor, rect, dpi: p.dpi, maximized: p.max })
            })
            .collect();
        let reopen = f
            .reopen
            .into_iter()
            .filter(|w| !w.is_empty())
            .take(REOPEN_WINDOWS)
            .map(|w| w.into_iter().take(REOPEN_TABS).collect())
            .collect();
        Session {
            placements,
            network_recent: f.network_recent.into_iter().take(NETWORK_MAX).collect(),
            known: f.known.into_iter().take(MAP_MAX).collect(),
            reopen,
        }
    }

    /// Writes the whole file; false when it could not be written.
    pub fn save(&self, own: &dyn OwnFiles) -> bool {
        let file = File {
            placements: self
                .placements
                .iter()
                .map(|(k, p)| {
                    let r = p.rect;
                    (
                        k.clone(),
                        Pl { monitor: p.monitor.clone(), l: r.l, t: r.t, r: r.r, b: r.b, dpi: p.dpi, max: p.maximized },
                    )
                })
                .collect(),
            network_recent: self.network_recent.clone(),
            known: self.known.clone(),
            reopen: self.reopen.clone(),
        };
        match serde_json::to_vec(&file) {
            Ok(bytes) => own.write_atomic(names::SESSION_FILE, &bytes),
            Err(_) => false,
        }
    }
}

/// The kit's placement store over Shared.session.
pub struct SessionStore {
    shared: Rc<Shared>,
}

impl SessionStore {
    pub fn new(shared: Rc<Shared>) -> SessionStore {
        SessionStore { shared }
    }
}

impl PlacementStore for SessionStore {
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        self.shared.session.borrow().placements.get(key.0).cloned()
    }

    /// Kept and written at once (the kit calls it at the end of a move or size and when the window is destroyed).
    fn save(&mut self, key: PlacementKey, p: &Placement) {
        let mut session = self.shared.session.borrow_mut();
        session.placements.insert(key.0.to_string(), p.clone());
        if !session.save(&*self.shared.services.own) {
            oled_sys::log!("files: {} was not written", names::SESSION_FILE);
        }
    }
}
