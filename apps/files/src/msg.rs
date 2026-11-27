//! What a worker thread or another window sends to a window (P3-files-app 2.2), through hub::Hub into Screen::data.
//! Every field is plain data, so a Msg crosses threads.

use std::sync::Arc;

use oled_trust::frame::files::{Change, InstanceCmd, OpId};

use crate::drives::DriveInfo;
use crate::listing::{Batch, Diff, Gen, SortDone, TabId, WidePath};
use crate::ops::classify::Recyclability;
use crate::services::ReqOwner;
use crate::shell_client::ShellReply;
use crate::watch::WatchEvent;

pub enum Msg {
    /// To the window that owns the tab (the hub's TabId -> win map).
    Batch { tab: TabId, batch: Batch },
    /// To the primary window only: ops, shell, watcher or class icons have something to drain.
    EnginesWake,
    /// To the window that ran the job (Bg::run's win; 0 = the primary window).
    Job { win: u32, done: JobDone },
    /// Engine output the primary window drained, re-posted to the window it belongs to (window::drain_engines).
    Fwd(Fwd),
    /// To the most recently active main window (Hub::set_active), else the primary.
    Instance(InstanceCmd),
    /// To every window.
    Updated { version: u32 },
    /// To every window: theme.json changed the suite language (the loop callback already called LoopCx::set_lang).
    Lang,
    /// A window to itself, posted from paint when its first full frame is done (F1a.10).
    FirstFrame,
}

pub enum Fwd {
    /// A reply to this window's own request (Icons, Menu, Exec).
    Shell { owner: ReqOwner, reply: ShellReply },
    /// An event of this window's watch (Shared.watch_owner); Drives and AssocChanged are handled process-wide.
    Watch(WatchEvent),
    /// Shared.ops changed: repaint the chip and the flyout, show this window's notices (OpsModel::take_notices).
    Ops,
    /// ItemsDone of any operation, to every main window: each applies what its tabs show (F3.10).
    Own { op: OpId, changes: Vec<Change> },
    /// Process-wide display data changed (type names, class icons, pins, drives): bump the visible tables, relayout.
    Bump,
}

pub enum JobDone {
    FreeSpace {
        root: WidePath,
        free: Option<u64>,
        total: Option<u64>,
    },
    /// Process-wide: the receiver updates Shared.drives and posts Fwd::Bump to every window.
    Drives(Vec<DriveInfo>),
    /// One drive finished within its 1.5 s budget (same rule).
    DriveFacts(DriveInfo),
    /// Tree, completion and siblings (folders only).
    Children {
        req: u64,
        parent: WidePath,
        dirs: Vec<Box<str>>,
        done: bool,
    },
    Facts {
        req: u64,
        items: Vec<(WidePath, Recyclability)>,
    },
    /// Watcher changes resolved off the UI thread.
    Resolved {
        tab: TabId,
        gen: Gen,
        diff: Diff,
    },
    /// found: Some(is_dir).
    Stat {
        req: u64,
        path: WidePath,
        found: Option<bool>,
    },
    Sorted {
        tab: TabId,
        gen: Gen,
        done: SortDone,
    },
    /// Stored in Shared.session.known.
    KnownFolders(Vec<(KnownName, WidePath)>),
    /// F1a.10 step 5, IconKey::Folder: (px, premultiplied BGRA).
    StockFolderIcon(Option<(u16, Arc<[u8]>)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownName {
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
    Profile,
}

/// p.send(Box::new(m)): false once the window is gone.
pub fn post_msg(p: &oled_ui::host::Proxy, m: Msg) -> bool {
    p.send(Box::new(m))
}

/// A Msg crosses threads (oled_ui::host::AppMsg is Box<dyn Any + Send>).
const _: fn() = || {
    fn is_send<T: Send>() {}
    is_send::<Msg>();
};
