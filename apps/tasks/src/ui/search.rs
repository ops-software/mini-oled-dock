//! Search: the matcher (case- and diacritic-insensitive, ANDed terms, an exact all-digit PID), typing in the list,
//! Esc and the empty state (T5.2, T5.3).

use std::collections::HashMap;

use oled_sysmon::ProcKey;
use oled_ui::model::{Align, RowKey};
use oled_ui::text::Trim;
use oled_ui::theme::TypeRole;
use oled_ui::{CommandId, EventCx, PaintCx, RectP};

use super::chrome::Chrome;
use super::rows::{Decor, RowEntry, RowKind};
use super::screen::TasksScreen;
use super::view::View;
use crate::app::resolver::NameBook;
use crate::strings::{k, t};

/// The base letter of every letter of Latin-1 Supplement and Latin Extended-A (U+00C0 to U+017F, lower case); '?' marks
/// the two signs that are not letters and stay as they are.
const BASE: &[u8; 192] = b"aaaaaaaceeeeiiiidnooooo?ouuuuytsaaaaaaaceeeeiiiidnooooo?ouuuuytyaaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiiiiijjkkkllllllllllnnnnnnnnnoooooooorrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs";

/// Separates the fields of a haystack, so that no term can match across two of them.
const FIELD_SEP: char = '\u{1}';

/// A text folded for matching: lower case, accents and combining marks dropped.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ('\u{300}'..='\u{36f}').contains(&ch) {
            continue;
        }
        for lc in ch.to_lowercase() {
            let c = lc as u32;
            if (0xC0..0x180).contains(&c) {
                match BASE[(c - 0xC0) as usize] {
                    b'?' => out.push(lc),
                    b => out.push(char::from(b)),
                }
            } else if !('\u{300}'..='\u{36f}').contains(&lc) {
                out.push(lc);
            }
        }
    }
    out
}

/// What a process's haystack was built from: if it is the same, the text is the same.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sig {
    exe: u32,
    name: u32,
    publisher: u32,
    services: u32,
}

struct Hay {
    sig: Sig,
    text: String,
}

/// The query and its compiled terms, and the haystacks of the processes.
#[derive(Default)]
pub struct Matcher {
    query: String,
    terms: Vec<String>,
    /// The PID an all-digit term stands for.
    pids: Vec<Option<u32>>,
    hay: HashMap<ProcKey, Hay>,
    /// (processes that match, processes) of the last filter.
    count: Option<(usize, usize)>,
    /// Scratch of the filter.
    index: HashMap<RowKey, usize>,
    keep: Vec<bool>,
}

impl Matcher {
    /// The terms of a query: split on white space, each folded; an all-digit term is also a PID.
    pub fn set_query(&mut self, q: &str) {
        self.query.clear();
        self.query.push_str(q);
        self.terms.clear();
        self.pids.clear();
        for word in q.split_whitespace() {
            let term = fold(word);
            if term.is_empty() {
                continue;
            }
            let pid =
                (term.bytes().all(|b| b.is_ascii_digit()) && term.len() <= 10).then(|| term.parse().ok()).flatten();
            self.pids.push(pid);
            self.terms.push(term);
        }
        if self.terms.is_empty() {
            self.hay = HashMap::new();
            self.count = None;
        }
    }

    /// The query as typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// (matching processes, processes) after the last filter, while a query stands.
    pub fn count(&self) -> Option<(usize, usize)> {
        self.count.filter(|_| self.stands())
    }

    fn stands(&self) -> bool {
        !self.terms.is_empty()
    }

    /// Whether a folded haystack of a process answers every term.
    fn answers(&self, text: &str, pid: u32) -> bool {
        self.terms.iter().zip(&self.pids).all(|(term, p)| *p == Some(pid) || text.contains(term.as_str()))
    }

    /// Builds the haystacks that are missing or stale (the names and services of a process change rarely), and forgets
    /// the processes that ended. Nothing runs without a query.
    pub fn prepare(&mut self, v: &View, names: &NameBook) {
        if !self.stands() {
            return;
        }
        let procs = v.procs();
        let services = &v.frame().services.entries;
        for (i, p) in procs.rows.iter().enumerate() {
            let exe = v.info_rows().get(i).filter(|x| x.key == p.key).and_then(|x| x.exe);
            let name = exe.and_then(|e| names.name_of_exe(e));
            let publisher = exe.and_then(|e| names.publisher_of(e));
            let lo = services.partition_point(|s| s.pid < p.key.pid);
            let hi = services.partition_point(|s| s.pid <= p.key.pid);
            let sig = Sig {
                exe: exe.map_or(0, |e| e.0.wrapping_add(1)),
                name: name.map_or(0, |n| n.len() as u32 + 1),
                publisher: publisher.map_or(0, |n| n.len() as u32 + 1),
                services: (hi - lo) as u32,
            };
            if self.hay.get(&p.key).is_some_and(|h| h.sig == sig) {
                continue;
            }
            let mut text = String::new();
            let mut field = |s: &str| {
                text.push_str(&fold(s));
                text.push(FIELD_SEP);
            };
            let mut image = String::new();
            v.proc_name(p.key, &mut image);
            field(&image);
            if let Some(n) = name {
                field(n);
            }
            if let Some(n) = publisher {
                field(n);
            }
            for s in &services[lo..hi] {
                field(&s.name);
                field(&s.display);
            }
            self.hay.insert(p.key, Hay { sig, text });
        }
        if self.hay.len() > procs.rows.len() * 2 + 16 {
            self.hay.retain(|k, _| procs.index_of(*k).is_some());
        }
    }

    fn proc_hit(&self, v: &View, key: ProcKey) -> bool {
        if let Some(h) = self.hay.get(&key) {
            return self.answers(&h.text, key.pid);
        }
        // An ended process kept as a ghost: its image name is all there is.
        let mut image = String::new();
        v.proc_name(key, &mut image);
        self.answers(&fold(&image), key.pid)
    }
}

/// True while a query stands (the empty state may show). T5.2.
pub fn query_stands(m: &Matcher) -> bool {
    m.stands()
}

/// The haystacks of the processes, ready for the filter of the same pass (names change, so it runs first).
pub fn prepare(v: &View, names: &NameBook, m: &mut Matcher) {
    m.prepare(v, names);
}

/// (matching processes, processes) of the last filter, while a query stands.
pub fn count(m: &Matcher) -> Option<(usize, usize)> {
    m.count()
}

/// Keeps the rows that match, with their sections and groups. Grouped and Tree are built open while a query stands, so
/// every process is a row: a group, a section or a Tree parent stays while a row under it does (and shows open); a row
/// that matches nothing goes. T5.2.
pub fn filter(v: &View, m: &mut Matcher, rows: &mut Vec<RowEntry>) {
    if !m.stands() {
        m.count = None;
        return;
    }
    let n = rows.len();
    m.index.clear();
    m.keep.clear();
    m.keep.resize(n, false);
    let mut matching = 0usize;
    for (i, r) in rows.iter().enumerate() {
        m.index.insert(r.key, i);
        if let RowKind::Proc(k) | RowKind::Ghost(k) = r.kind {
            if m.proc_hit(v, k) {
                m.keep[i] = true;
                matching += usize::from(matches!(r.kind, RowKind::Proc(_)));
            }
        }
    }
    // A kept row keeps its ancestors: a parent always comes before its children, so one pass from the end is enough.
    for i in (0..n).rev() {
        if m.keep[i] {
            if let Some(&j) = rows[i].parent.and_then(|p| m.index.get(&p)) {
                m.keep[j] = true;
            }
        }
    }
    // Sections and groups that stay show open, their section count says how many groups stay.
    let mut under: HashMap<RowKey, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if m.keep[i] {
            if let Some(p) = r.parent {
                *under.entry(p).or_default() += 1;
            }
        }
    }
    for (i, r) in rows.iter_mut().enumerate() {
        if !m.keep[i] {
            continue;
        }
        match r.kind {
            RowKind::Section(_) => {
                r.expanded = true;
                let n = under.get(&r.key).copied().unwrap_or(0);
                v.set_decor(r.key, Decor { head: String::new(), tail: format!(" ({n})"), hung: false });
            }
            RowKind::Group(_) | RowKind::Proc(_) if r.expandable => r.expanded = true,
            _ => {}
        }
    }
    let mut at = 0;
    rows.retain(|_| {
        let keep = m.keep[at];
        at += 1;
        keep
    });
    m.count = Some((matching, v.procs().rows.len()));
}

/// The field's text changed. T5.2.
pub fn on_query(m: &mut Matcher, q: &str) {
    m.set_query(q);
}

/// Esc clears the search first; true when it did (the screen filters again). T5.3. An open IME composition is the
/// field's to cancel first.
pub fn on_esc(cx: &mut EventCx, m: &mut Matcher, chrome: &mut Chrome) -> bool {
    if m.query.is_empty() || chrome.composing() {
        return false;
    }
    m.set_query("");
    chrome.clear_field(cx);
    true
}

/// A printable key or "/" in the list starts a search; true when taken (the screen filters again). T5.3. "/" only
/// moves the focus to the field.
pub fn on_list_text(cx: &mut EventCx, m: &mut Matcher, chrome: &mut Chrome, c: char) -> bool {
    if c == '/' {
        return chrome.focus_field(cx);
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    match chrome.type_into_field(cx, c) {
        Some(text) => {
            m.set_query(&text);
            true
        }
        None => false,
    }
}

/// "Nothing matches" over the empty list. T5.3.
pub fn paint_empty(m: &Matcher, cx: &mut PaintCx, viewport: RectP) {
    let lang = cx.lang();
    let line = oled_sys::i18n::subst(&t(lang, k::TASKS_SEARCH_NONE), &[("q", m.query.trim())]);
    let hint = t(lang, k::TASKS_SEARCH_NONE_HINT);
    let (secondary, tertiary) = {
        let c = &cx.tokens().color;
        (c.fg_secondary, c.fg_tertiary)
    };
    let mid = viewport.t + viewport.h() / 2;
    let (h1, h2) = (cx.px(24), cx.px(20));
    let first = RectP { l: viewport.l, t: mid - h1, r: viewport.r, b: mid };
    let second = RectP { l: viewport.l, t: mid + cx.px(2), r: viewport.r, b: mid + cx.px(2) + h2 };
    for (text, role, rect, ink) in
        [(line.as_str(), TypeRole::Body, first, secondary), (&*hint, TypeRole::Caption, second, tertiary)]
    {
        // The width is learnt by drawing once where nothing shows, then the line is drawn centred.
        let mut width = 0;
        cx.with_clip(RectP::EMPTY, &mut |c| width = c.text_in(text, role, rect, Align::Start, Trim::End, ink));
        let l = rect.l + ((rect.w() - width) / 2).max(0);
        cx.text_in(text, role, RectP { l, ..rect }, Align::Start, Trim::End, ink);
    }
}

/// FIND: the field takes the focus with its text selected. T5.3.
pub fn on_command(s: &mut TasksScreen, cx: &mut EventCx, _id: CommandId) {
    s.chrome.select_field(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(m: &Matcher, text: &str, pid: u32) -> bool {
        m.answers(&fold(text), pid)
    }

    fn query(q: &str) -> Matcher {
        let mut m = Matcher::default();
        m.set_query(q);
        m
    }

    #[test]
    fn fold_table_is_complete() {
        assert_eq!(BASE.len(), 192);
        assert_eq!(fold("Citt\u{e0}"), "citta");
        assert_eq!(fold("\u{c9}COLE \u{17d}ivot"), "ecole zivot");
        // A combining mark after the letter is dropped; the sign x of multiplication stays.
        assert_eq!(fold("e\u{301}"), "e");
        assert_eq!(fold("a\u{d7}b"), "a\u{d7}b");
        assert!(BASE.iter().all(|b| b.is_ascii()));
    }

    #[test]
    fn terms_are_anded_and_folded() {
        let m = query("  ORBIT quill ");
        let hay = format!("orbit.exe{FIELD_SEP}Quill Ltd{FIELD_SEP}");
        assert!(hit(&m, &hay, 1));
        assert!(!hit(&query("orbit zeta"), &hay, 1));
        assert!(hit(&query("Citt\u{e0}"), "citta", 1));
        assert!(hit(&query("citta"), "Citt\u{e0}", 1));
        assert!(!query("   ").stands(), "white space alone is no query");
    }

    #[test]
    fn pid_is_exact() {
        let m = query("1234");
        assert!(hit(&m, "orbit", 1234));
        assert!(!hit(&m, "orbit", 12345));
        assert!(hit(&m, "tool1234", 7), "a name that holds the digits matches too");
        assert!(hit(&query("orbit 1234"), "orbit", 1234), "ANDed with a name term");
        assert!(!hit(&query("zeta 1234"), "orbit", 1234));
    }

    #[test]
    fn a_term_does_not_cross_fields() {
        let m = query("exeorbit");
        assert!(!hit(&m, &format!("a.exe{FIELD_SEP}orbit{FIELD_SEP}"), 1));
    }
}
