//! Tree mode: the parent forest and the ancestor chain.
use oled_sysmon::{ProcKey, ProcTable};

pub const NONE: u32 = u32::MAX;

/// Longest ancestor chain.
const MAX_CHAIN: usize = 64;

/// Indices into ProcTable.rows.
#[derive(Clone, Debug, Default)]
pub struct Forest {
    pub roots: Vec<u32>,
    pub first_child: Vec<u32>,
    pub next_sibling: Vec<u32>,
}

/// Children by created ascending; a row with parent None is a root.
pub fn forest(procs: &ProcTable, out: &mut Forest) {
    let n = procs.rows.len();
    out.roots.clear();
    out.first_child.clear();
    out.first_child.resize(n, NONE);
    out.next_sibling.clear();
    out.next_sibling.resize(n, NONE);
    let mut parent_of: Vec<u32> = Vec::with_capacity(n);
    for (i, row) in procs.rows.iter().enumerate() {
        let parent = row.parent.and_then(|k| procs.index_of(k)).filter(|&p| p != i);
        parent_of.push(parent.map_or(NONE, |p| p as u32));
        if parent.is_none() {
            out.roots.push(i as u32);
        }
    }
    // Visit the rows from the newest to the oldest and push each at the head of its parent's list: every list
    // ends up in created order, in one pass after one sort.
    let mut newest_first: Vec<u32> = (0..n as u32).collect();
    newest_first.sort_unstable_by(|&a, &b| {
        let (ka, kb) = (procs.rows[a as usize].key, procs.rows[b as usize].key);
        (kb.created, kb.pid).cmp(&(ka.created, ka.pid))
    });
    for i in newest_first {
        let p = parent_of[i as usize];
        if p != NONE {
            out.next_sibling[i as usize] = out.first_child[p as usize];
            out.first_child[p as usize] = i;
        }
    }
}

/// Clears out; from, then its parent links (finish's CreateTime rule), at most 64 keys; empty if from is absent.
pub fn ancestors(procs: &ProcTable, from: ProcKey, out: &mut Vec<ProcKey>) {
    out.clear();
    let mut k = Some(from);
    while let Some(key) = k {
        let Some(row) = procs.find(key) else { break };
        if out.len() >= MAX_CHAIN {
            break;
        }
        out.push(key);
        k = row.parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oled_sysmon::ProcRow;

    fn k(pid: u32, created: u64) -> ProcKey {
        ProcKey { pid, created }
    }

    fn table(rows: &[(u32, u64, u32)]) -> ProcTable {
        let mut t = ProcTable::new();
        for &(pid, created, parent_pid) in rows {
            t.push(ProcRow { key: k(pid, created), parent_pid, ..ProcRow::default() });
        }
        t.finish();
        t
    }

    fn children(f: &Forest, i: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut c = f.first_child[i as usize];
        while c != NONE {
            v.push(c);
            c = f.next_sibling[c as usize];
        }
        v
    }

    #[test]
    fn forest_children_by_creation() {
        // Pid order differs from creation order among the children of pid 10.
        let t = table(&[(10, 100, 0), (30, 300, 10), (20, 500, 10), (40, 200, 10), (50, 600, 20)]);
        let mut f = Forest::default();
        forest(&t, &mut f);
        let pid = |i: u32| t.rows[i as usize].key.pid;
        assert_eq!(f.roots.iter().map(|&i| pid(i)).collect::<Vec<_>>(), vec![10]);
        let root = f.roots[0];
        assert_eq!(children(&f, root).iter().map(|&i| pid(i)).collect::<Vec<_>>(), vec![40, 30, 20]);
        let twenty = t.index_of(k(20, 500)).unwrap() as u32;
        assert_eq!(children(&f, twenty).iter().map(|&i| pid(i)).collect::<Vec<_>>(), vec![50]);
        assert!(children(&f, t.index_of(k(50, 600)).unwrap() as u32).is_empty());
    }

    #[test]
    fn orphans_are_roots() {
        // Pid 7's parent pid 99 is not in the table; pid 8 names a parent created after it (a reused pid).
        let t = table(&[(7, 100, 99), (8, 100, 9), (9, 200, 0)]);
        let mut f = Forest::default();
        forest(&t, &mut f);
        assert_eq!(f.roots.len(), 3);
        assert!(f.first_child.iter().all(|&c| c == NONE));
        // A second call over a smaller table leaves nothing of the first.
        let small = table(&[(7, 100, 0)]);
        forest(&small, &mut f);
        assert_eq!((f.roots.len(), f.first_child.len(), f.next_sibling.len()), (1, 1, 1));
    }

    #[test]
    fn forest_is_linear() {
        let rows: Vec<(u32, u64, u32)> = (1..=10_000u32).map(|p| (p, p as u64 * 10, p - 1)).collect();
        let t = table(&rows);
        let mut f = Forest::default();
        forest(&t, &mut f);
        assert_eq!(f.roots, vec![0]);
        let mut depth = 0;
        let mut cur = 0u32;
        while f.first_child[cur as usize] != NONE {
            cur = f.first_child[cur as usize];
            depth += 1;
        }
        assert_eq!(depth, 9_999);
    }

    #[test]
    fn ancestors_follow_parent_links() {
        let t = table(&[(1, 10, 0), (2, 20, 1), (3, 30, 2), (4, 40, 3), (5, 50, 4)]);
        let mut out = vec![k(99, 99)];
        ancestors(&t, k(5, 50), &mut out);
        assert_eq!(out, vec![k(5, 50), k(4, 40), k(3, 30), k(2, 20), k(1, 10)]);
        // A recycled parent pid with a later created time was never linked, so the chain ends there.
        let t = table(&[(1, 100, 0), (2, 200, 1), (3, 300, 2), (4, 50, 3)]);
        ancestors(&t, k(4, 50), &mut out);
        assert_eq!(out, vec![k(4, 50)]);
        ancestors(&t, k(3, 300), &mut out);
        assert_eq!(out, vec![k(3, 300), k(2, 200), k(1, 100)]);
    }

    #[test]
    fn ancestors_stop_at_64() {
        let rows: Vec<(u32, u64, u32)> = (1..=100u32).map(|p| (p, p as u64 * 10, p - 1)).collect();
        let t = table(&rows);
        let mut out = Vec::new();
        ancestors(&t, k(100, 1000), &mut out);
        assert_eq!(out.len(), 64);
        assert_eq!(out[0], k(100, 1000));
        assert_eq!(out[63], k(37, 370));
    }

    #[test]
    fn ancestors_of_an_absent_key_is_empty() {
        let t = table(&[(1, 10, 0)]);
        let mut out = vec![k(1, 10)];
        ancestors(&t, k(2, 20), &mut out);
        assert!(out.is_empty());
        ancestors(&t, k(1, 11), &mut out);
        assert!(out.is_empty());
    }
}
