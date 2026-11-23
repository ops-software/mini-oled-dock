//! Identity keys of the hosted tray icons in `config.tray_hidden` and `config.tray_order` (pure, tested).
//!
//! An icon with a GUID is keyed by that GUID (lowercase, no braces), as before. An icon without one is keyed
//! `<lowercase exe>|<n>` (`key_of`): n is its 0-based place among that exe's GUID-less icons whose owner
//! window is alive, in registration order. Icons with no bitmap yet and hidden ones count too, so the place does
//! not jump while an icon loads; icons of a dead owner are numbered after the live ones, so keys stay unique.
//! An unknown exe is "?". When an earlier icon of the exe goes, the later ones move up one place (accepted: one
//! click hides an icon again).
//!
//! Releases before 1.5.1 keyed these icons `<exe>#<uID>`, and an app that picks a new uID at every start never
//! matched its entries twice. `migrate` rewrites those entries in place the first time the exe's icons are
//! seen, exact uID matches first, then by position; nothing is dropped. A release before 1.5.1 does not know
//! the new keys: there, the GUID-less icons hidden after the migration show again and those ordered after it go
//! back to the default order. Icons with a GUID are not affected.

use crate::config::{Config, TRAY_ORDER_CAP};

/// The two config lists that hold icon keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Lists {
    pub(super) hidden: Vec<String>,
    pub(super) order: Vec<String>,
}

impl Lists {
    pub(super) fn from_config(c: &Config) -> Lists {
        Lists { hidden: c.tray_hidden.clone(), order: c.tray_order.clone() }
    }

    /// Writes both lists into `c` and nothing else (the order capped at `TRAY_ORDER_CAP`, as a drag saves it).
    pub(super) fn write_back(self, c: &mut Config) {
        c.tray_hidden = self.hidden;
        c.tray_order = self.order;
        c.tray_order.truncate(TRAY_ORDER_CAP);
    }
}

/// A live GUID-less icon of `exe`: its uID now and its place (its key is `key_of(exe, ordinal)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Seen {
    pub(super) exe: String,
    pub(super) uid: u32,
    pub(super) ordinal: usize,
}

/// The exe part of a key: lowercase, "?" when unknown.
fn exe_part(exe: &str) -> String {
    if exe.is_empty() {
        "?".to_string()
    } else {
        exe.to_lowercase()
    }
}

/// Key of the GUID-less icon `ordinal` of `exe`.
pub(super) fn key_of(exe: &str, ordinal: usize) -> String {
    format!("{}|{ordinal}", exe_part(exe))
}

/// Keys of the icons `(exe, GUID key, owner alive)`, in order: an icon with a GUID gets that string unchanged and
/// takes no place; the others `key_of` their exe, the live ones numbered first, then the dead ones after them.
pub(super) fn keys_of<S: AsRef<str>>(items: &[(S, Option<String>, bool)]) -> Vec<String> {
    let mut keys: Vec<String> = items.iter().map(|(_, guid, _)| guid.clone().unwrap_or_default()).collect();
    // (exe part, next place); a handful of exes, so a scan.
    let mut next: Vec<(String, usize)> = Vec::new();
    for live in [true, false] {
        for ((exe, guid, alive), key) in items.iter().zip(keys.iter_mut()) {
            if guid.is_some() || *alive != live {
                continue;
            }
            let part = exe_part(exe.as_ref());
            let n = match next.iter_mut().find(|(p, _)| *p == part) {
                Some((_, n)) => {
                    *n += 1;
                    *n - 1
                }
                None => {
                    next.push((part.clone(), 1));
                    0
                }
            };
            *key = format!("{part}|{n}");
        }
    }
    keys
}

/// `<exe><sep><digits>`: (lowercase exe, number); the exe part is never empty.
fn split_key(entry: &str, sep: char) -> Option<(String, usize)> {
    let (exe, n) = entry.rsplit_once(sep)?;
    if exe.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((exe.to_lowercase(), n.parse().ok()?))
}

/// `<exe>#<uID>`, the key of a GUID-less icon before 1.5.1: (lowercase exe, uID). None for anything else: a bare
/// GUID, a new key, the unknown exe "?", an exe with "|", text a hand edit left.
fn legacy(entry: &str) -> Option<(String, u32)> {
    let (exe, uid) = split_key(entry, '#')?;
    if exe == "?" || exe.contains('|') {
        return None;
    }
    Some((exe, u32::try_from(uid).ok()?))
}

/// `<exe>|<n>`, a key of this release: (lowercase exe, n).
fn new_key(entry: &str) -> Option<(String, usize)> {
    split_key(entry, '|')
}

fn is_legacy_of(entry: &str, exe: &str) -> bool {
    legacy(entry).is_some_and(|(x, _)| x == exe)
}

/// Removes `o` from `v`; true if it was there.
fn take(v: &mut Vec<usize>, o: usize) -> bool {
    match v.iter().position(|&x| x == o) {
        Some(j) => {
            v.remove(j);
            true
        }
        None => false,
    }
}

/// The places 0..k of `exe` (lowercase) that have no `exe|n` entry in `list`.
fn without_entry(list: &[String], exe: &str, k: usize) -> Vec<usize> {
    let present: Vec<usize> =
        list.iter().filter_map(|e| new_key(e).filter(|(x, _)| x == exe).map(|(_, n)| n)).collect();
    (0..k).filter(|o| !present.contains(o)).collect()
}

/// One pass for one exe (lowercase), whose live GUID-less icons are its entries in `seen`. Returns the entries
/// replaced or inserted.
fn migrate_exe(l: &mut Lists, exe: &str, seen: &[Seen]) -> usize {
    let icons = || seen.iter().filter(move |s| s.exe.to_lowercase() == exe);
    let k = icons().count();
    let place_of = |uid: u32| icons().find(|s| s.uid == uid).map(|s| s.ordinal);
    let mut n = 0;

    // tray_hidden: an exact uID first, then by position; the legacy entries still left wait for a later icon.
    let mut free = without_entry(&l.hidden, exe, k);
    if !free.is_empty() {
        let mut rest = Vec::new();
        for (i, entry) in l.hidden.iter_mut().enumerate() {
            let Some((x, uid)) = legacy(entry) else { continue };
            if x != exe {
                continue;
            }
            match place_of(uid) {
                Some(o) if take(&mut free, o) => {
                    *entry = key_of(exe, o);
                    n += 1;
                }
                _ => rest.push(i),
            }
        }
        for (i, o) in rest.into_iter().zip(free) {
            l.hidden[i] = key_of(exe, o);
            n += 1;
        }
    }

    // tray_order: an exact uID first; the places still missing take the first legacy entry's place, else go right
    // after the exe's last new key, else stay unordered (the bar appends them). The list never grows past
    // TRAY_ORDER_CAP here, so write_back's cap never drops a key this added (the next pass would add it again).
    let mut missing = without_entry(&l.order, exe, k);
    if missing.is_empty() {
        return n;
    }
    for entry in l.order.iter_mut() {
        let Some((x, uid)) = legacy(entry) else { continue };
        if x != exe {
            continue;
        }
        match place_of(uid) {
            Some(o) if take(&mut missing, o) => {
                *entry = key_of(exe, o);
                n += 1;
            }
            _ => {}
        }
    }
    if missing.is_empty() {
        return n;
    }
    let room = TRAY_ORDER_CAP.saturating_sub(l.order.len());
    if let Some(p) = l.order.iter().position(|e| is_legacy_of(e, exe)) {
        // The first key takes the entry's place, which needs no room.
        for (j, &o) in missing.iter().take(room + 1).enumerate() {
            if j == 0 {
                l.order[p] = key_of(exe, o);
            } else {
                l.order.insert(p + j, key_of(exe, o));
            }
            n += 1;
        }
    } else if let Some(p) = l.order.iter().rposition(|e| new_key(e).is_some_and(|(x, _)| x == exe)) {
        for (j, &o) in missing.iter().take(room).enumerate() {
            l.order.insert(p + 1 + j, key_of(exe, o));
            n += 1;
        }
    }
    n
}

/// Rewrites the legacy entries of the exes in `seen` (and orders their new places) as the module doc says. Returns
/// the number of entries replaced or inserted, 0 when nothing changed; the same input twice returns 0 the second
/// time. Entries of exes not in `seen`, bare GUIDs and the unknown exe are left alone.
pub(super) fn migrate(l: &mut Lists, seen: &[Seen]) -> usize {
    // One pass settles every case but one: a full tray_order, where the places that did not fit take the exe's
    // next legacy entries in the passes after. A pass that changes something replaces a legacy entry or grows the
    // order towards its cap, so this ends; the bound only guards that argument.
    let bound = l.hidden.len() + l.order.len() + TRAY_ORDER_CAP + 1;
    let mut total = 0;
    for _ in 0..bound {
        let mut n = 0;
        for (i, s) in seen.iter().enumerate() {
            let exe = s.exe.to_lowercase();
            // Each exe once, at its first icon; the unknown exe never.
            if exe.is_empty() || exe == "?" || seen[..i].iter().any(|t| t.exe.to_lowercase() == exe) {
                continue;
            }
            n += migrate_exe(l, &exe, seen);
        }
        if n == 0 {
            break;
        }
        total += n;
    }
    total
}

/// true when `migrate` would change the lists.
pub(super) fn needs_migration(l: &Lists, seen: &[Seen]) -> bool {
    !seen.is_empty() && migrate(&mut l.clone(), seen) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn lists(hidden: &[&str], order: &[&str]) -> Lists {
        Lists { hidden: s(hidden), order: s(order) }
    }

    fn seen(exe: &str, uid: u32, ordinal: usize) -> Seen {
        Seen { exe: exe.into(), uid, ordinal }
    }

    #[test]
    fn key_format() {
        assert_eq!(key_of("GaugeTool.exe", 0), "gaugetool.exe|0");
        assert_eq!(key_of("", 0), "?|0");
        assert_eq!(key_of("a.exe", 12), "a.exe|12");
    }

    #[test]
    fn ordinals_are_per_exe_in_registration_order() {
        let g = "7820ae73-23e3-4229-82c1-e41cb67d5b9c";
        let items = [
            ("a.exe", None, true),
            ("b.exe", None, true),
            ("a.exe", None, true),
            ("a.exe", Some(g.into()), true),
            ("A.EXE", None, true),
        ];
        assert_eq!(keys_of(&items), s(&["a.exe|0", "b.exe|0", "a.exe|1", g, "a.exe|2"]));
        assert_eq!(
            keys_of(&[("", None, true), ("", None, true)]),
            s(&["?|0", "?|1"]),
            "an unknown exe is numbered too"
        );
        assert!(keys_of::<&str>(&[]).is_empty());
    }

    #[test]
    fn dead_owner_does_not_take_an_ordinal() {
        let items = [("x.exe", None, false), ("x.exe", None, true), ("y.exe", None, false), ("x.exe", None, true)];
        assert_eq!(keys_of(&items), s(&["x.exe|2", "x.exe|0", "y.exe|0", "x.exe|1"]));
    }

    #[test]
    fn new_uid_every_start_is_migrated_once() {
        let mut l = lists(&["gauge.exe#1001"], &["lights.exe#3", "gauge.exe#1002", "games.exe#1", "gauge.exe#1003"]);
        let now = |uid: u32| [seen("Lights.exe", 3, 0), seen("games.exe", 1, 0), seen("Gauge.exe", uid, 0)];
        assert!(needs_migration(&l, &now(7001)));
        assert_eq!(migrate(&mut l, &now(7001)), 4);
        let want = lists(&["gauge.exe|0"], &["lights.exe|0", "gauge.exe|0", "games.exe|0", "gauge.exe#1003"]);
        assert_eq!(l, want);
        // Every later start picks a new uID: the same key, nothing to migrate, the lists byte-identical.
        for uid in [7001, 7002, 7003, 7004, 7005] {
            assert_eq!(keys_of(&[("Gauge.exe", None, true)]), s(&["gauge.exe|0"]));
            assert!(!needs_migration(&l, &now(uid)), "uid {uid}");
            assert_eq!(migrate(&mut l, &now(uid)), 0, "uid {uid}");
            assert_eq!(l, want);
        }
    }

    #[test]
    fn two_icons_registered_one_after_the_other() {
        let mut l = lists(&["x#1", "x#2", "y#7"], &[]);
        assert_eq!(migrate(&mut l, &[seen("x", 5, 0)]), 1);
        assert_eq!(l.hidden, s(&["x|0", "x#2", "y#7"]));
        let both = [seen("x", 5, 0), seen("x", 6, 1)];
        assert_eq!(migrate(&mut l, &both), 1);
        assert_eq!(l.hidden, s(&["x|0", "x|1", "y#7"]));
        assert_eq!(migrate(&mut l, &both), 0);
        let mut at_once = lists(&["x#1", "x#2", "y#7"], &[]);
        assert_eq!(migrate(&mut at_once, &both), 2);
        assert_eq!(at_once, l);
    }

    #[test]
    fn exact_uid_match_wins() {
        let two = [seen("x", 1, 0), seen("x", 2, 1)];
        let mut l = lists(&["x#2"], &[]);
        assert_eq!(migrate(&mut l, &two), 1);
        assert_eq!(l.hidden, s(&["x|1"]));
        // In tray_order too; the place still missing goes right after it.
        let mut l = lists(&[], &["a", "x#2", "b"]);
        assert_eq!(migrate(&mut l, &two), 2);
        assert_eq!(l.order, s(&["a", "x|1", "x|0", "b"]));
    }

    #[test]
    fn stale_legacy_waits_without_loss() {
        let mut l = lists(&["x#1", "x#2"], &["x#1", "x#2"]);
        let one = [seen("x", 9, 0)];
        assert_eq!(migrate(&mut l, &one), 2);
        let waiting = lists(&["x|0", "x#2"], &["x|0", "x#2"]);
        assert_eq!(l, waiting);
        for _ in 0..3 {
            assert_eq!(migrate(&mut l, &one), 0);
        }
        assert_eq!(l, waiting);
        // A second icon, later: the entries that waited take it.
        assert_eq!(migrate(&mut l, &[seen("x", 9, 0), seen("x", 10, 1)]), 2);
        assert_eq!(l, lists(&["x|0", "x|1"], &["x|0", "x|1"]));
    }

    #[test]
    fn order_grows_as_icons_arrive() {
        let mut l = lists(&[], &["a#9", "b#1"]);
        assert_eq!(migrate(&mut l, &[seen("a", 3, 0)]), 1);
        assert_eq!(l.order, s(&["a|0", "b#1"]));
        assert_eq!(migrate(&mut l, &[seen("a", 3, 0), seen("a", 4, 1)]), 1);
        assert_eq!(l.order, s(&["a|0", "a|1", "b#1"]));
        assert_eq!(migrate(&mut l, &[seen("b", 2, 0)]), 1);
        assert_eq!(l.order, s(&["a|0", "a|1", "b|0"]));
        // An exe the user never ordered stays out of the list (the bar appends it).
        assert_eq!(migrate(&mut l, &[seen("c", 1, 0)]), 0);
    }

    #[test]
    fn second_legacy_order_entry_is_consumed_by_the_second_icon() {
        let mut l = lists(&[], &["x#1", "y|0", "x#2"]);
        assert_eq!(migrate(&mut l, &[seen("x", 7, 0)]), 1);
        assert_eq!(l.order, s(&["x|0", "y|0", "x#2"]));
        assert_eq!(migrate(&mut l, &[seen("x", 7, 0), seen("x", 8, 1)]), 1);
        assert_eq!(l.order, s(&["x|0", "y|0", "x|1"]));
    }

    #[test]
    fn bare_guid_entries_are_untouched() {
        let g1 = "7820ae73-23e3-4229-82c1-e41cb67d5b9c";
        let g2 = "{7820AE73-23E3-4229-82C1-E41CB67D5B9C}";
        let mut l = lists(&[g1, "x#1", g2], &[g2, "x#1", g1]);
        assert_eq!(migrate(&mut l, &[seen("x", 1, 0)]), 2);
        assert_eq!(l, lists(&[g1, "x|0", g2], &[g2, "x|0", g1]));
    }

    #[test]
    fn unknown_exe_is_not_migrated() {
        let odd = ["?#3", "#3", "x#", "x#3a", "x|y#3", "x#99999999999", "x", "x|", "|3"];
        let mut l = lists(&odd, &odd);
        let before = l.clone();
        assert_eq!(migrate(&mut l, &[seen("", 3, 0), seen("?", 3, 0)]), 0);
        assert_eq!(migrate(&mut l, &[seen("x", 3, 0)]), 0, "none of these is an entry of x");
        assert_eq!(migrate(&mut l, &[seen("z", 3, 0)]), 0, "an exe with no entry");
        assert_eq!(l, before);
        assert!(!needs_migration(&l, &[]));
    }

    #[test]
    fn a_full_order_never_grows_past_its_cap() {
        let fill = |n: usize| (0..n).map(|i| format!("f{i}.exe|0")).collect::<Vec<_>>();
        let three = [seen("x", 1, 0), seen("x", 2, 1), seen("x", 3, 2)];
        // Full, the places that do not fit take the exe's next legacy entries; nothing is pushed out.
        let mut order = fill(TRAY_ORDER_CAP - 2);
        order.extend(s(&["x#5", "x#6"]));
        let mut l = Lists { hidden: Vec::new(), order };
        assert_eq!(migrate(&mut l, &three), 2);
        assert_eq!(l.order.len(), TRAY_ORDER_CAP);
        assert_eq!(l.order[TRAY_ORDER_CAP - 2..], s(&["x|0", "x|1"]));
        assert_eq!(migrate(&mut l, &three), 0);
        // One free slot: the legacy entry's place plus one more.
        let mut order = fill(TRAY_ORDER_CAP - 2);
        order.push("x#5".into());
        let mut l = Lists { hidden: Vec::new(), order };
        assert_eq!(migrate(&mut l, &three), 2);
        assert_eq!(l.order[TRAY_ORDER_CAP - 2..], s(&["x|0", "x|1"]));
        assert_eq!(migrate(&mut l, &three), 0);
        let mut c = Config::default();
        l.clone().write_back(&mut c);
        assert_eq!(c.tray_order, l.order, "the cap of write_back drops nothing the migration added");
    }

    #[test]
    fn write_back_touches_only_the_two_lists() {
        let mut c = Config::default();
        c.extra.insert("from_a_newer_release".into(), serde_json::json!({ "k": [1, 2] }));
        c.tray_hidden = s(&["x#1", "7820ae73-23e3-4229-82c1-e41cb67d5b9c"]);
        c.tray_order = (0..TRAY_ORDER_CAP + 5).map(|i| format!("o{i}")).collect();
        let mut want = c.clone();
        let mut l = Lists::from_config(&c);
        assert_eq!(migrate(&mut l, &[seen("x", 1, 0)]), 1);
        l.write_back(&mut c);
        want.tray_hidden[0] = "x|0".into();
        want.tray_order.truncate(TRAY_ORDER_CAP);
        assert_eq!(c, want, "every other field, and the keys of newer releases, as they were");
    }

    /// xorshift64*: the same cases on every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn is_subsequence(part: &[&String], whole: &[String]) -> bool {
        let mut it = whole.iter();
        part.iter().all(|p| it.any(|w| w == *p))
    }

    #[test]
    fn random_lists_never_panic_and_settle() {
        let long = "n".repeat(300);
        let exes =
            ["a.exe", "B.exe", "b.exe", "x|y.exe", "x#y.exe", "", "?", long.as_str(), "é.exe", "日本.exe", "İ.exe"];
        let odd = ["", "#", "|", "#1", "|1", "a.exe#", "a.exe|", "a.exe#1x", "a.exe#99999999999", "a.exe||1"];
        let entry = |rng: &mut Rng| -> String {
            let exe = exes[rng.below(exes.len())];
            match rng.below(8) {
                0..=2 => format!("{exe}#{}", rng.below(6)),
                3 | 4 => format!("{}|{}", exe.to_lowercase(), rng.below(4)),
                5 => "7820ae73-23e3-4229-82c1-e41cb67d5b9c".to_string(),
                6 => odd[rng.below(odd.len())].to_string(),
                _ => exe.to_string(),
            }
        };
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for case in 0..20_000 {
            let hidden_len = rng.below(10);
            let order_len = if rng.below(40) == 0 { TRAY_ORDER_CAP - 8 + rng.below(16) } else { rng.below(12) };
            let mut l = Lists {
                hidden: (0..hidden_len).map(|_| entry(&mut rng)).collect(),
                order: (0..order_len).map(|_| entry(&mut rng)).collect(),
            };
            let mut seen: Vec<Seen> = Vec::new();
            for _ in 0..rng.below(5) {
                let exe = exes[rng.below(exes.len())];
                let ordinal = seen.iter().filter(|s| s.exe.to_lowercase() == exe.to_lowercase()).count();
                seen.push(Seen { exe: exe.to_string(), uid: rng.below(6) as u32, ordinal });
            }
            let before = l.clone();
            let n = migrate(&mut l, &seen);
            assert_eq!(n == 0, l == before, "case {case}: the count says whether anything changed");
            assert_eq!(l.hidden.len(), before.hidden.len(), "case {case}: hidden keeps every entry");
            assert!(l.order.len() >= before.order.len(), "case {case}");
            assert!(l.order.len() <= before.order.len().max(TRAY_ORDER_CAP), "case {case}");
            let seen_exe = |e: &String| legacy(e).is_some_and(|(x, _)| seen.iter().any(|s| s.exe.to_lowercase() == x));
            for (a, b) in before.hidden.iter().zip(&l.hidden) {
                assert!(a == b || seen_exe(a), "case {case}: {a} became {b}");
            }
            let kept: Vec<&String> = before.order.iter().filter(|e| !seen_exe(e)).collect();
            assert!(is_subsequence(&kept, &l.order), "case {case}: an entry of the order was lost or moved");
            for (old, new) in [(&before.hidden, &l.hidden), (&before.order, &l.order)] {
                for k in new.iter().filter(|e| new_key(e).is_some()) {
                    let count = |v: &Vec<String>| v.iter().filter(|e| *e == k).count();
                    assert!(count(new) <= count(old).max(1), "case {case}: {k} written twice");
                }
            }
            let settled = l.clone();
            assert!(!needs_migration(&l, &seen), "case {case}");
            assert_eq!(migrate(&mut l, &seen), 0, "case {case}: a second call changes nothing");
            assert_eq!(l, settled);
        }
    }
}
