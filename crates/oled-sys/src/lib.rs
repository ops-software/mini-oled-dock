//! Win32 plumbing shared by the dock and the apps.
// ---- L-B (this plan)
pub mod i18n;
pub mod logger;
pub mod winhttp;
// ---- L-P (plumbing): pipe, worker, admin, journal_os, process identity, single instance, datadir for apps
pub mod darkmenu;
pub mod datadir;
pub mod ident;
pub mod journal_os;
pub mod lang;
pub mod pipe;
pub mod selftest;
pub mod sha256;
pub mod worker;
// ---- L-T, L-F: below their own markers when their plans add modules
// ---- L-T
pub mod admin;
