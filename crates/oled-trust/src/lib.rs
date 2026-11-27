//! Pure trust code: frame codecs, journal format and undo planner, manifest and version rules. No OS calls.
#![forbid(unsafe_code)]
// ---- L-F (files engine): pub mod frame;          (framing, peer checks, verb enums; L-T adds the AdminRequest set inside frame)
pub mod frame;
// ---- L-P (plumbing)
pub mod journal;
pub mod manifest;
