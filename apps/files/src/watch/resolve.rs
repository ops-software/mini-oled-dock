//! Raw changes to a store Diff. Runs off the UI thread: it stats every name that was added, modified or renamed to.

use std::collections::HashMap;

use super::{ChangeAction, RawChange};
use crate::listing::{stat_one, Diff, Gen, WidePath};

/// The diff that brings a store of `dir` up to date with `items` (in the order they arrived).
///
/// Each name is looked at once, by its last change: Removed and RenamedFrom remove it; Added, Modified and RenamedTo
/// read it from the file system and upsert what is there. A name that is gone at stat time (created and deleted
/// inside the delivery window) is removed, which the store treats as a no-op when it never had it. A name that
/// cannot be read (an error, not an absence) is left out: the next change of that name, or a rescan, repairs it.
pub fn resolve(dir: &WidePath, gen: Gen, items: &[RawChange]) -> Diff {
    let mut last: Vec<(&[u8], ChangeAction)> = Vec::new();
    let mut at: HashMap<&[u8], usize> = HashMap::new();
    for c in items {
        match at.get(&*c.name) {
            Some(&i) => last[i].1 = c.action,
            None => {
                at.insert(&c.name, last.len());
                last.push((&c.name, c.action));
            }
        }
    }
    let mut diff = Diff::new(gen);
    for (name, action) in last {
        match action {
            ChangeAction::Removed | ChangeAction::RenamedFrom => diff.remove(name),
            ChangeAction::Added | ChangeAction::Modified | ChangeAction::RenamedTo => match stat_one(dir, name) {
                Ok(Some(found)) => diff.upsert(&found.name, found.e),
                Ok(None) => diff.remove(name),
                Err(_) => {}
            },
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(action: ChangeAction, name: &str) -> RawChange {
        RawChange { action, name: name.as_bytes().into() }
    }

    fn missing_dir() -> WidePath {
        WidePath::parse(r"C:\files-engine-resolve-test-no-such-folder").unwrap()
    }

    #[test]
    fn removals_need_no_stat() {
        let items = [ch(ChangeAction::Removed, "a"), ch(ChangeAction::RenamedFrom, "b")];
        let d = resolve(&missing_dir(), Gen(4), &items);
        assert_eq!(d.gen, Gen(4));
        assert_eq!((d.upserts.len(), d.removes.len(), d.rescan), (0, 2, false));
    }

    #[test]
    fn a_name_is_looked_at_once_by_its_last_change() {
        let items = [
            ch(ChangeAction::Added, "x"),
            ch(ChangeAction::Modified, "x"),
            ch(ChangeAction::Removed, "x"),
            ch(ChangeAction::Removed, "y"),
            ch(ChangeAction::Added, "y"), // absent in the missing folder: a remove, not an upsert
        ];
        let d = resolve(&missing_dir(), Gen(1), &items);
        assert_eq!(d.upserts.len(), 0);
        assert_eq!(d.removes.len(), 2, "one remove per name");
    }

    #[test]
    fn an_absent_name_becomes_a_remove() {
        let d = resolve(&missing_dir(), Gen(1), &[ch(ChangeAction::Added, "gone.txt")]);
        assert_eq!((d.upserts.len(), d.removes.len()), (0, 1));
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(resolve(&missing_dir(), Gen(1), &[]).is_empty());
    }
}
