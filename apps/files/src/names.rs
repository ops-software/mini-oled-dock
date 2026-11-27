//! The window's fixed names (P3-files-app 2.1): window classes, the single-instance names, the identity, its own files,
//! sizes, exit codes and the timer kinds. Other plans read them (the dock's instance sender, the shortcuts' AUMID);
//! names_are_pinned keeps every value.

use oled_ui::host::{AppId, PlacementKey};
use oled_ui::SizeL;

/// Every main window (ROADMAP X11).
pub const WINDOW_CLASS: &str = "MiniOledFiles";
/// The 360 px operations window (F3.5). Not "MiniOledFilesOps" (the ops host's hidden window) nor "MiniOledFilesShell"
/// (the shell host's).
pub const OPS_WINDOW_CLASS: &str = "MiniOledFilesProgress";
/// The message-only window, one per UI process (X10, X11, F-P7).
pub const IPC_CLASS: &str = "MiniOledFilesIpc";
/// The single-instance mutex (F-P7).
pub const MUTEX: &str = r"Local\mini-oled-dock.files";
/// Debug builds only: "<MUTEX>-<tag>" and "<IPC_CLASS>-<tag>" (instance::InstanceNames).
pub const TEST_TAG_VAR: &str = "FILES_TEST_TAG";
/// Debug builds only: --forward-only found no instance.
pub const EXIT_NOT_FORWARDED: i32 = 7;
/// SetCurrentProcessExplicitAppUserModelID before the first window (X11); P2-plumbing's shortcut writes the same.
pub const AUMID: &str = "MiniOledDock.Files";
pub const APP: AppId = AppId("files");
pub const MAIN_PLACEMENT: PlacementKey = PlacementKey("files.main");
/// The title says Beta and About names the stage; the stable release's plan sets it false.
pub const STAGE_BETA: bool = true;
/// The options file in the suite's roaming folder (FileKind::PerUser, ROADMAP X8).
pub const OPTIONS_FILE: &str = "files.json";
/// The own data folder's files (own_files::OwnDir).
pub const SESSION_FILE: &str = "session.json";
pub const TYPES_FILE: &str = "types.json";
/// The loop guard (SPEC-files 5.5.7).
pub const GUARD_FILE: &str = "files-guard.json";
pub const DEFAULT_SIZE: SizeL = SizeL { w: 1100, h: 720 };
pub const MIN_SIZE: SizeL = SizeL { w: 560, h: 360 };
/// Logical px between further windows.
pub const CASCADE: i32 = 24;
pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ELEVATED_HANDOFF: i32 = 5;

/// Timer kinds (window::timer): 1-5 the navigation's page states, then one kind per owner.
pub const TIMER_NAV_FIRST: u8 = 1;
pub const TIMER_NAV_LAST: u8 = 5;
pub const TIMER_CHIP: u8 = 6;
pub const TIMER_LINGER: u8 = 7;
pub const TIMER_PRESPAWN: u8 = 8;
pub const TIMER_NOTICE: u8 = 9;
