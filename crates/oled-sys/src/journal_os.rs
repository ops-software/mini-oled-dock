//! The OS side of the change journal: registry, tasks, files and the locked journal file (F-P5).
// PF1 freeze: the signatures below are frozen; bodies arrive with J1.
#![allow(dead_code, unused_variables)]

use oled_trust::journal::{Digest, Hive, OsErr, RegData, RegPath, RegSnapshot, TaskName};

pub struct RealOs {
    _private: (),
}
impl RealOs {
    pub fn for_scope(scope: oled_trust::journal::Scope) -> RealOs {
        unimplemented!("PF1 stub")
    }
}
impl oled_trust::journal::JournalOs for RealOs {
    // Reg*W calls; tasks through schtasks.exe /Query /XML, /Create /XML /F, /Delete /F; digest = sha256
    fn reg_get(&self, hive: Hive, key: &RegPath, name: Option<&str>) -> Result<Option<RegData>, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_set(&mut self, hive: Hive, key: &RegPath, name: Option<&str>, data: Option<&RegData>) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_key_exists(&self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_key_create(&mut self, hive: Hive, key: &RegPath) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_key_delete_if_empty(&mut self, hive: Hive, key: &RegPath) -> Result<bool, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_tree_get(&self, hive: Hive, key: &RegPath) -> Result<Option<RegSnapshot>, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn reg_tree_put(&mut self, hive: Hive, key: &RegPath, snap: Option<&RegSnapshot>) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn task_xml(&self, name: &TaskName) -> Result<Option<String>, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn task_put(&mut self, name: &TaskName, xml: Option<&str>) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn file_digest(&self, path: &str) -> Result<Option<Digest>, OsErr> {
        unimplemented!("PF1 stub")
    }
    fn file_delete(&mut self, path: &str) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn file_restore(&mut self, dest: &str, from: &str) -> Result<(), OsErr> {
        unimplemented!("PF1 stub")
    }
    fn digest(&self, bytes: &[u8]) -> Result<Digest, OsErr> {
        unimplemented!("PF1 stub")
    }
}
pub struct FileSink {
    _private: (),
}
impl oled_trust::journal::Sink for FileSink {
    fn append(&mut self, bytes: &[u8]) -> Result<(), oled_trust::journal::OsErr> {
        unimplemented!("PF1 stub")
    }
}
impl FileSink {
    pub fn delete_if_settled(self, j: &oled_trust::journal::Journal) -> Result<bool, oled_trust::journal::OsErr> {
        unimplemented!("PF1 stub")
    }
} // under the lock
pub fn path(scope: oled_trust::journal::Scope) -> Option<std::path::PathBuf> {
    unimplemented!("PF1 stub")
} // User: %LOCALAPPDATA%\mini-oled-dock\journal\user.jsonl; Machine: oled_sys::datadir::journal_dir()\machine.jsonl
pub fn update_journal_path() -> Option<std::path::PathBuf> {
    unimplemented!("PF1 stub")
} // <root>\update.jnl (Scope::Machine format)
pub fn open_locked(
    scope: oled_trust::journal::Scope,
    timeout: std::time::Duration,
) -> Result<(oled_trust::journal::Journal, FileSink), oled_trust::journal::OsErr> {
    unimplemented!("PF1 stub")
}
pub fn open_locked_at(
    path: &std::path::Path,
    scope: oled_trust::journal::Scope,
    timeout: std::time::Duration,
) -> Result<(oled_trust::journal::Journal, FileSink), oled_trust::journal::OsErr> {
    unimplemented!("PF1 stub")
}
pub fn load(scope: oled_trust::journal::Scope) -> Result<oled_trust::journal::Journal, String> {
    unimplemented!("PF1 stub")
} // read-only snapshot for lists only
pub fn scope_allowed(scope: oled_trust::journal::Scope, elevated: bool) -> bool {
    unimplemented!("PF1 stub")
} // User needs !elevated; Machine needs elevated
