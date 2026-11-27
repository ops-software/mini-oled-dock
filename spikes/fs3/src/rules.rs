//! The pass rules of the FS-3 rows (plans P1-files-engine, SP1 and LIVE-F1), as pure functions so each one is
//! tested here and every record states the rule it applied.

/// Row e1: CANCEL_IN_OP is true when PerformOperations returned within 1 s of the cancel and at most 11 files
/// reached the destination.
pub fn cancel_in_op(cancel_ms: Option<u64>, return_ms: u64, files_at_dst: usize) -> bool {
    matches!(cancel_ms, Some(c) if return_ms.saturating_sub(c) <= 1000) && files_at_dst <= 11
}

/// Row e2: the copy completed after the block: success, nothing aborted, every file at the destination, and the
/// callback really blocked (at least 4.5 s of the 5 s).
pub fn pause_completed(
    ok: bool,
    aborted: Option<bool>,
    files_at_dst: usize,
    expected: usize,
    block: Option<(u64, u64)>,
) -> bool {
    ok && aborted == Some(false) && files_at_dst == expected && matches!(block, Some((a, b)) if b >= a + 4500)
}

/// Row e2w: no window of the probe was visible while the callback blocked (a window that appeared before the block
/// ended and was not gone before it began counts).
pub fn no_window_during(windows: &[(u64, Option<u64>)], block: Option<(u64, u64)>) -> bool {
    match block {
        None => false,
        Some((a, b)) => !windows.iter().any(|&(seen, gone)| seen <= b && gone.is_none_or(|g| g >= a)),
    }
}

/// Row a: DIALOGS_UNDER_SILENT is true when a dialog appeared within 3 s of the start of PerformOperations and the
/// destination's bytes did not change.
pub fn dialogs_under_silent(dialog_ms: Option<u64>, perform_start_ms: u64, hash_equal: bool) -> bool {
    hash_equal && matches!(dialog_ms, Some(d) if d.saturating_sub(perform_start_ms) <= 3000)
}

/// Row b: OWNER_ACROSS_PROCESSES is true when the dialog appeared while the owner window's thread was still hung
/// and 5 of 5 WM_NULL sent to the dialog were answered within 500 ms.
pub fn owner_across_processes(dialog_ms: Option<u64>, owner_hung_until_ms: u64, answered: usize) -> bool {
    answered == 5 && matches!(dialog_ms, Some(d) if d < owner_hung_until_ms)
}

/// Row g: RESTORE_FROM_BIN is true when the file really went to the bin (it left its folder and psiNewlyCreated lies
/// in the bin folder), the move back succeeded and brought it back with equal bytes, and the Recycle Bin's item count
/// is back at its start value. A run that never recycled anything proves nothing: false.
pub fn restore_from_bin(recycled: bool, restored_equal: bool, bin_start: Option<i64>, bin_end: Option<i64>) -> bool {
    recycled && restored_equal && bin_start.is_some() && bin_start == bin_end
}

/// Row i: the verbs of the file's menu the shell host intercepts.
const FILE_VERBS: [&str; 4] = ["delete", "cut", "copy", "rename"];

fn has_verb(v: &[String], w: &str) -> bool {
    v.iter().any(|x| x.eq_ignore_ascii_case(w))
}

/// Row i: the verbs the shell host intercepts are readable: delete, cut, copy and rename on the file's menu, paste
/// on the folder's.
pub fn menu_pass(file_verbs: &[String], folder_verbs: &[String]) -> bool {
    FILE_VERBS.iter().all(|w| has_verb(file_verbs, w)) && has_verb(folder_verbs, "paste")
}

/// Row i's verdict: true when `menu_pass` holds. The default menu drops Paste when the clipboard holds nothing to
/// paste, so when the file's four verbs read back and only the folder's paste is missing while the clipboard holds no
/// files, the row is not measured (None, with the reason in the list). Any other miss is false.
pub fn menu_verdict(
    file_verbs: &[String],
    folder_verbs: &[String],
    clipboard_has_files: bool,
) -> (Option<bool>, Vec<&'static str>) {
    let file_ok = FILE_VERBS.iter().all(|w| has_verb(file_verbs, w));
    match (file_ok, has_verb(folder_verbs, "paste"), clipboard_has_files) {
        (true, true, _) => (Some(true), vec![]),
        (true, false, false) => (None, vec!["paste: clipboard holds no files"]),
        _ => (Some(false), vec![]),
    }
}

/// Row c: what a Recycle of one file did. "FAIL" is the data-loss case: gone, not in the bin, and no dialog asked.
pub fn delete_outcome(dialog: bool, still_at_source: bool, in_bin: bool) -> &'static str {
    match (still_at_source, in_bin, dialog) {
        (true, _, true) => "kept, after a dialog",
        (true, _, false) => "kept",
        (false, true, _) => "recycled",
        (false, false, true) => "deleted, after a dialog",
        (false, false, false) => "FAIL",
    }
}

/// Row c: whether a case reproduced what the job set up. c3 (a bin smaller than the file) and c4 (a disabled bin)
/// that end "recycled" show the bin value had no effect. c1 and c2 test a kind of volume and always count.
pub fn reproduced(case: &str, outcome: &str) -> bool {
    !(matches!(case, "c3" | "c4") && outcome == "recycled")
}

/// Row c's verdict over its cases, each (name, outcome when one was measured, reproduced): false when any case lost
/// data ("FAIL"); true only when every case was measured and reproduced; otherwise not measured (None). The list names
/// the cases that were not measured or not reproduced, so a record never reads as a pass by default.
pub fn no_data_loss<'a>(cases: &[(&'a str, Option<&str>, bool)]) -> (Option<bool>, Vec<&'a str>) {
    let gaps: Vec<&'a str> = cases.iter().filter(|(_, o, r)| o.is_none() || !r).map(|(n, _, _)| *n).collect();
    if cases.iter().any(|(_, o, _)| *o == Some("FAIL")) {
        (Some(false), gaps)
    } else if gaps.is_empty() && !cases.is_empty() {
        (Some(true), gaps)
    } else {
        (None, gaps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e1_cancel_rule() {
        assert!(cancel_in_op(Some(100), 1100, 9));
        assert!(cancel_in_op(Some(100), 300, 11));
        assert!(!cancel_in_op(Some(100), 1101, 9), "returned late");
        assert!(!cancel_in_op(Some(100), 200, 12), "too many files");
        assert!(!cancel_in_op(None, 200, 0), "the cancel never happened");
    }

    #[test]
    fn e2_pause_rules() {
        assert!(pause_completed(true, Some(false), 64, 64, Some((100, 5100))));
        assert!(!pause_completed(true, Some(false), 63, 64, Some((100, 5100))));
        assert!(!pause_completed(true, Some(true), 64, 64, Some((100, 5100))));
        assert!(!pause_completed(true, Some(false), 64, 64, Some((100, 1100))), "did not block");
        assert!(!pause_completed(false, Some(false), 64, 64, Some((100, 5100))));
        assert!(no_window_during(&[], Some((100, 5100))));
        assert!(no_window_during(&[(6000, Some(6100))], Some((100, 5100))), "after the block");
        assert!(no_window_during(&[(10, Some(50))], Some((100, 5100))), "gone before the block");
        assert!(!no_window_during(&[(2000, Some(2100))], Some((100, 5100))));
        assert!(!no_window_during(&[(10, None)], Some((100, 5100))), "still open from before");
        assert!(!no_window_during(&[], None), "never blocked");
    }

    #[test]
    fn a_b_g_i_c_rules() {
        assert!(dialogs_under_silent(Some(2500), 100, true));
        assert!(!dialogs_under_silent(Some(3200), 100, true), "too late");
        assert!(!dialogs_under_silent(Some(500), 100, false), "the destination changed");
        assert!(!dialogs_under_silent(None, 100, true));
        assert!(owner_across_processes(Some(1000), 20_000, 5));
        assert!(!owner_across_processes(Some(1000), 20_000, 4));
        assert!(!owner_across_processes(Some(21_000), 20_000, 5), "appeared only after the owner woke");
        assert!(!owner_across_processes(None, 20_000, 0));
        assert!(restore_from_bin(true, true, Some(3), Some(3)));
        assert!(!restore_from_bin(true, true, Some(3), Some(4)));
        assert!(!restore_from_bin(true, false, Some(3), Some(3)));
        assert!(!restore_from_bin(true, true, None, None));
        assert!(!restore_from_bin(false, true, Some(3), Some(3)), "never recycled: the file and the count never moved");
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(menu_pass(&v(&["open", "Delete", "cut", "copy", "rename"]), &v(&["paste"])));
        assert!(!menu_pass(&v(&["delete", "cut", "copy"]), &v(&["paste"])));
        assert!(!menu_pass(&v(&["delete", "cut", "copy", "rename"]), &v(&["open"])));
        assert_eq!(delete_outcome(false, false, false), "FAIL");
        assert_eq!(delete_outcome(true, false, false), "deleted, after a dialog");
        assert_eq!(delete_outcome(false, false, true), "recycled");
        assert_eq!(delete_outcome(true, true, false), "kept, after a dialog");
    }

    #[test]
    fn c_verdict_needs_every_case_measured_and_reproduced() {
        assert!(reproduced("c1", "recycled") && reproduced("c2", "recycled"), "c1 and c2 test a kind of volume");
        assert!(!reproduced("c3", "recycled") && !reproduced("c4", "recycled"), "the bin value had no effect");
        assert!(reproduced("c3", "deleted, after a dialog") && reproduced("c4", "kept, after a dialog"));
        let all = [("c1", Some("kept"), true), ("c2", Some("recycled"), true), ("c3", Some("kept"), true)];
        assert_eq!(no_data_loss(&all), (Some(true), vec![]));
        let lost = [("c1", Some("FAIL"), true), ("c2", None, false)];
        assert_eq!(no_data_loss(&lost), (Some(false), vec!["c2"]), "data loss is a result even with gaps");
        let errored = [("c1", None, false), ("c2", None, false), ("c3", None, false), ("c4", None, false)];
        assert_eq!(no_data_loss(&errored), (None, vec!["c1", "c2", "c3", "c4"]), "nothing measured is no pass");
        let vacuous = [("c3", Some("recycled"), false), ("c4", Some("kept"), true)];
        assert_eq!(no_data_loss(&vacuous), (None, vec!["c3"]), "a bin value without effect is not reproduced");
        assert_eq!(no_data_loss(&[]), (None, vec![]));
    }

    #[test]
    fn i_verdict_tells_an_empty_clipboard_from_an_unread_verb() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let file = v(&["open", "delete", "cut", "copy", "rename"]);
        assert_eq!(menu_verdict(&file, &v(&["open", "paste"]), false), (Some(true), vec![]));
        assert_eq!(menu_verdict(&file, &v(&["open", "paste"]), true), (Some(true), vec![]));
        assert_eq!(
            menu_verdict(&file, &v(&["open"]), false),
            (None, vec!["paste: clipboard holds no files"]),
            "the default menu drops Paste when there is nothing to paste"
        );
        assert_eq!(menu_verdict(&file, &v(&["open"]), true), (Some(false), vec![]), "files to paste and no paste read");
        let no_rename = v(&["delete", "cut", "copy"]);
        assert_eq!(menu_verdict(&no_rename, &v(&["open"]), false), (Some(false), vec![]), "a file verb is missing");
        assert_eq!(menu_verdict(&no_rename, &v(&["paste"]), true), (Some(false), vec![]));
        assert_eq!(menu_verdict(&[], &[], false), (Some(false), vec![]), "no menu at all");
    }
}
