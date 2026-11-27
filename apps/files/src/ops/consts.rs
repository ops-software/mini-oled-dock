//! The constants spike FS-3 decides (OP3). Each keeps its safe value until the orchestrator's one-line commit after
//! LIVE-F1 (plan 5.1) changes it; nothing else changes them.

/// FS-3 a: the system's replace/skip dialog appears under FOF_SILENT. False: "Decide each" with collisions runs
/// without FOF_SILENT (flags.rs).
pub const DIALOGS_UNDER_SILENT: bool = false;
/// FS-3 b: a window of the UI process can own the dialogs of the host's operations. False: the host's own hidden
/// window owns them.
pub const OWNER_ACROSS_PROCESSES: bool = false;
/// FS-3 e1: ERROR_CANCELLED from the sink stops PerformOperations within a second. False: chunks of 64 top-level
/// items, cancel between chunks.
pub const CANCEL_IN_OP: bool = false;
/// FS-3 e2: the sink may block a callback (pause) without a window appearing. False: Pause is refused.
pub const PAUSE_OK: bool = false;
/// FS-3 g: a recycled item can be moved back from the bin by its parsing name. False: undo of a Recycle opens the
/// Recycle Bin.
pub const RESTORE_FROM_BIN: bool = false;

/// Top-level items per IFileOperation (open point 15): a cancel point between chunks, and the host's memory bound.
pub const CHUNK_ITEMS: usize = if CANCEL_IN_OP { 256 } else { 64 };
/// Staged items held in memory per operation; the rest spill to disk (journal.rs).
pub const STAGE_IN_MEMORY: usize = 4_096;
/// Staged items one operation may hold at most; the next Stage frame is refused (TooMany).
pub const STAGE_MAX: u64 = 1_000_000;
/// Operations running at once; more queue FIFO.
pub const MAX_RUNNING: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_defaults_until_live_f1() {
        let all = [DIALOGS_UNDER_SILENT, OWNER_ACROSS_PROCESSES, CANCEL_IN_OP, PAUSE_OK, RESTORE_FROM_BIN];
        assert_eq!(all, [false; 5], "the FS-3 constants change only by the orchestrator's commit after LIVE-F1");
        assert_eq!(CHUNK_ITEMS, 64);
        assert_eq!((STAGE_IN_MEMORY, STAGE_MAX, MAX_RUNNING), (4_096, 1_000_000, 4));
    }
}
