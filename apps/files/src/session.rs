//! session.json (P3-files-app 2.2; FA-WIN fills it, F1a.10): placements, recent network places, known folders and
//! the windows to reopen, through Services.own.

use std::collections::BTreeMap;
use std::rc::Rc;

use oled_ui::host::{Placement, PlacementKey, PlacementStore};

use crate::services::OwnFiles;
use crate::window::Shared;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub placements: BTreeMap<String, Placement>,
    pub network_recent: Vec<(String, i64)>,
    pub known: BTreeMap<String, String>,
    /// At most 8 windows x 8 tabs (F4.14).
    pub reopen: Vec<Vec<String>>,
}

impl Session {
    /// Read capped at 16 KiB; a malformed file gives defaults. Filled by FA-WIN; until then defaults.
    pub fn load(own: &dyn OwnFiles) -> Session {
        let _ = own;
        Session::default()
    }

    /// Filled by FA-WIN; until then nothing is written.
    pub fn save(&self, own: &dyn OwnFiles) -> bool {
        let _ = own;
        false
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
    /// Filled by FA-WIN; until then none.
    fn load(&self, key: PlacementKey) -> Option<Placement> {
        let _ = (key, &self.shared);
        None
    }

    /// Filled by FA-WIN; until then nothing.
    fn save(&mut self, key: PlacementKey, p: &Placement) {
        let _ = (key, p);
    }
}
