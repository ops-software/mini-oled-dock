//! Folding and ranking of the launcher (SPEC 5.2.5): `fold`, the name index built once at load, the match tiers 0-6
//! and the destructive default rule of 5.2.7. Tier 7 (Windows' typo rewrite) is not here: it reads a file and so runs
//! on the shell thread (L1a-12g).

use super::keywords;
use super::model::{Item, Payload, Query, Score};
use crate::start_search::Keys;
use core::cmp::Reverse;

/// The base letters of U+00C0..=U+017F, one entry per code point; '\0' keeps the character as it is
/// (the multiplication and division signs, which the calculator reads).
const LATIN: [&str; 192] = [
    // U+00C0..U+00FF
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "ss", //
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "y", //
    // U+0100..U+017F
    "a", "a", "a", "a", "a", "a", "c", "c", "c", "c", "c", "c", "c", "c", "d", "d", //
    "d", "d", "e", "e", "e", "e", "e", "e", "e", "e", "e", "e", "g", "g", "g", "g", //
    "g", "g", "g", "g", "h", "h", "h", "h", "i", "i", "i", "i", "i", "i", "i", "i", //
    "i", "i", "ij", "ij", "j", "j", "k", "k", "k", "l", "l", "l", "l", "l", "l", "l", //
    "l", "l", "l", "n", "n", "n", "n", "n", "n", "n", "n", "n", "o", "o", "o", "o", //
    "o", "o", "oe", "oe", "r", "r", "r", "r", "r", "r", "s", "s", "s", "s", "s", "s", //
    "s", "s", "t", "t", "t", "t", "t", "t", "u", "u", "u", "u", "u", "u", "u", "u", //
    "u", "u", "u", "u", "w", "w", "y", "y", "y", "z", "z", "z", "z", "z", "z", "s", //
];

/// Appends `s` folded to `out`: lowercase, with the accented Latin letters of U+00C0..=U+017F reduced to their base
/// letters ("Attività" becomes "attivita", the sharp s becomes "ss"). Callers clear `out` first when they reuse it.
pub fn fold(s: &str, out: &mut String) {
    for c in s.chars() {
        fold_char(c, out);
    }
}

fn fold_char(c: char, out: &mut String) {
    let cp = c as u32;
    if (0xC0..=0x17F).contains(&cp) {
        match LATIN.get((cp - 0xC0) as usize) {
            Some(&"\0") | None => out.push(c),
            Some(base) => out.push_str(base),
        }
    } else {
        out.extend(c.to_lowercase());
    }
}

// ---------------------------------------------------------------- the name index

/// A name folded once, with the start offsets of its words and their initials (5.2.5). Built at load, never per
/// keystroke. Words are split on non-alphanumerics and on a lower-to-upper case change ("VisualStudio").
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Indexed {
    pub folded: String,
    /// Byte offsets into `folded` where a word starts.
    pub words: Vec<u16>,
    pub initials: String,
}

impl Indexed {
    pub fn new(name: &str) -> Indexed {
        let mut x = Indexed::default();
        x.reset(name);
        x
    }

    /// The index of another name, built in place: the buffers are kept, so a provider that indexes names per
    /// keystroke (window names and titles, the commands in the current language) allocates nothing once they are
    /// large enough.
    pub fn reset(&mut self, name: &str) {
        self.folded.clear();
        self.words.clear();
        self.initials.clear();
        let mut in_word = false;
        let mut prev: Option<char> = None;
        for c in name.chars() {
            if c.is_alphanumeric() {
                let starts = !in_word || prev.is_some_and(|p| p.is_lowercase() && c.is_uppercase());
                let before = self.folded.len();
                if starts {
                    if let Ok(off) = u16::try_from(before) {
                        self.words.push(off);
                    }
                }
                fold_char(c, &mut self.folded);
                if starts {
                    if let Some(first) = self.folded.get(before..).and_then(|t| t.chars().next()) {
                        self.initials.push(first);
                    }
                }
                in_word = true;
            } else {
                fold_char(c, &mut self.folded);
                in_word = false;
            }
            prev = Some(c);
        }
    }

    pub fn cand<'a>(&'a self, keys: &'a Keys, extra: &'a [&'static str], rank: u32) -> Cand<'a> {
        Cand { folded: &self.folded, words: &self.words, initials: &self.initials, keys, extra, rank }
    }
}

/// One candidate for matching: all of it borrowed from data built at load.
#[derive(Clone, Copy)]
pub struct Cand<'a> {
    pub folded: &'a str,
    /// Word start offsets into `folded`.
    pub words: &'a [u16],
    pub initials: &'a str,
    /// Windows' synonyms (start_search), already lowercase.
    pub keys: &'a Keys,
    /// Our hidden keywords (the bilingual table, a command's words), already folded.
    pub extra: &'a [&'static str],
    /// Microsoft's popularity rank, smaller is better.
    pub rank: u32,
}

impl Cand<'_> {
    fn any_keyword(&self, pred: impl Fn(&str) -> bool) -> bool {
        self.extra.iter().any(|k| pred(k)) || self.keys.iter().any(|(k, _)| pred(k))
    }

    /// The order key of 5.4.1 for a match of `tier`. `kind` is the row kind's rank (app, window, command...).
    pub fn score(&self, tier: u8, running: bool, kind: u8) -> Score {
        let len = u16::try_from(self.folded.chars().count()).unwrap_or(u16::MAX);
        Score { tier, learned: Reverse(0), idle: !running, kind, rank: self.rank, len }
    }
}

// ---------------------------------------------------------------- tiers

fn contains_every_word(c: &Cand, q: &str) -> bool {
    q.split_whitespace().all(|w| c.folded.contains(w))
}

/// Tiers 0-5 of 5.2.5 for the folded query `q`: the best tier that matches, or None. An empty query matches nothing
/// (the empty state shows no rows). Tier 6 is `match_fuzzy`, which the caller asks only when no candidate matched.
pub fn match_tiers(c: &Cand, q: &str) -> Option<u8> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let tier = tiers(c, q);
    // The tiers tried: up to the one that matched, all six when none did (the bench's linear bound, LC-3).
    #[cfg(test)]
    TIER_CHECKS.with(|n| n.set(n.get() + tier.map_or(6, |t| u64::from(t) + 1)));
    tier
}

#[cfg(test)]
thread_local! {
    /// The tier checks `match_tiers` made on this thread. Per thread, like the allocation counter of the test kit, so
    /// the tests running beside a measurement never add to it.
    pub(crate) static TIER_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `match_tiers` for a trimmed query that is not empty.
fn tiers(c: &Cand, q: &str) -> Option<u8> {
    let chars = q.chars().count();
    if c.folded == q {
        return Some(0);
    }
    if c.folded.starts_with(q) || c.any_keyword(|k| k == q) {
        return Some(1);
    }
    let every = contains_every_word(c, q);
    if every {
        let first = q.split_whitespace().next().unwrap_or(q);
        let word_start = c.words.iter().any(|&w| c.folded.get(usize::from(w)..).is_some_and(|t| t.starts_with(first)));
        if word_start {
            return Some(2);
        }
    }
    if chars >= 2 && !q.contains(char::is_whitespace) && c.initials.contains(q) {
        return Some(3);
    }
    if every {
        return Some(4);
    }
    if chars >= 2 && c.any_keyword(|k| k.starts_with(q)) {
        return Some(5);
    }
    None
}

/// The caller's policy in one place: every candidate through tiers 0-5; only when none matched, through tier 6.
/// Pushes (index, tier) into `out` (cleared first).
#[cfg(test)]
pub fn match_all(cands: &[Cand], q: &str, out: &mut Vec<(usize, u8)>) {
    out.clear();
    out.extend(cands.iter().enumerate().filter_map(|(i, c)| match_tiers(c, q).map(|t| (i, t))));
    if out.is_empty() {
        out.extend(cands.iter().enumerate().filter_map(|(i, c)| match_fuzzy(c, q).map(|t| (i, t))));
    }
}

const FUZZY_MAX_Q: usize = 32;
const FUZZY_BUF: usize = FUZZY_MAX_Q + 3;

/// Tier 6: the query is a subsequence of the name with at most 2 gaps (3+ characters), or within Damerau distance 1
/// (query of 4+ characters; 2 at 8+) of a word prefix of the name or of one of its keywords. Allocates nothing.
pub fn match_fuzzy(c: &Cand, q: &str) -> Option<u8> {
    let q = q.trim();
    let mut qc = ['\0'; FUZZY_MAX_Q];
    let mut m = 0;
    for ch in q.chars() {
        let slot = qc.get_mut(m)?;
        *slot = ch;
        m += 1;
    }
    let qc = qc.get(..m)?;
    let dist = match m {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    };
    let compact = |t: &str| subsequence_gaps(t, qc, 2);
    if m >= 3 && qc.iter().all(|ch| !ch.is_whitespace()) && compact(c.folded) {
        return Some(6);
    }
    if dist == 0 {
        return None;
    }
    let name = c.words.iter().filter_map(|&w| c.folded.get(usize::from(w)..)).any(|t| near_prefix(t, qc, dist));
    let key = |k: &str| {
        near_prefix(k, qc, dist)
            || k.match_indices(' ').any(|(i, _)| k.get(i + 1..).is_some_and(|t| near_prefix(t, qc, dist)))
    };
    if name || c.any_keyword(key) {
        return Some(6);
    }
    None
}

/// `q` runs through `t` in order with at most `max_gaps` runs of skipped characters inside the match (greedy from every
/// place the first character occurs).
fn subsequence_gaps(t: &str, q: &[char], max_gaps: u8) -> bool {
    let Some((&first, rest)) = q.split_first() else { return false };
    t.char_indices().filter(|&(_, ch)| ch == first).any(|(at, ch)| {
        let mut want = rest.iter();
        let mut next = want.next();
        let mut gaps = 0u8;
        let mut in_gap = false;
        for ch in t.get(at + ch.len_utf8()..).unwrap_or("").chars() {
            let Some(&n) = next else { break };
            if ch == n {
                next = want.next();
                in_gap = false;
            } else if !in_gap {
                in_gap = true;
                gaps += 1;
                if gaps > max_gaps {
                    return false;
                }
            }
        }
        next.is_none()
    })
}

/// Is some prefix of `t` (the length of `q` plus or minus `dist`) within Damerau (optimal string alignment) distance
/// `dist` of `q`?
fn near_prefix(t: &str, q: &[char], dist: usize) -> bool {
    let mut buf = ['\0'; FUZZY_BUF];
    let mut n = 0;
    for ch in t.chars().take(q.len() + dist) {
        if let Some(slot) = buf.get_mut(n) {
            *slot = ch;
            n += 1;
        }
    }
    let Some(buf) = buf.get(..n) else { return false };
    (q.len().saturating_sub(dist).max(1)..=n).any(|len| buf.get(..len).is_some_and(|p| osa(q, p) <= dist))
}

/// Optimal string alignment distance (insert, delete, replace, swap of neighbours), three rows on the stack.
fn osa(a: &[char], b: &[char]) -> usize {
    type Row = [u8; FUZZY_BUF + 1];
    fn get(r: &Row, i: usize) -> u8 {
        r.get(i).copied().unwrap_or(u8::MAX)
    }
    fn set(r: &mut Row, i: usize, v: u8) {
        if let Some(s) = r.get_mut(i) {
            *s = v;
        }
    }
    let mut older: Row = [0; FUZZY_BUF + 1];
    let mut prev: Row = [0; FUZZY_BUF + 1];
    for j in 0..=b.len() {
        set(&mut prev, j, j as u8);
    }
    for (i, &ac) in a.iter().enumerate() {
        let mut cur: Row = [0; FUZZY_BUF + 1];
        set(&mut cur, 0, (i + 1) as u8);
        for (j, &bc) in b.iter().enumerate() {
            let sub = get(&prev, j) + u8::from(ac != bc);
            let mut best = sub.min(get(&prev, j + 1) + 1).min(get(&cur, j) + 1);
            let swapped = i > 0 && j > 0 && a.get(i - 1) == Some(&bc) && b.get(j - 1) == Some(&ac);
            if swapped {
                best = best.min(get(&older, j - 1) + 1);
            }
            set(&mut cur, j + 1, best);
        }
        older = prev;
        prev = cur;
    }
    usize::from(get(&prev, b.len()))
}

// ---------------------------------------------------------------- destructive rule

/// 5.2.7: may a destructive row be the default selection for the folded query `q`? Only when `q` equals one of its
/// `names` (name and aliases, folded), or has 4+ characters and is a prefix of a name or of a word of a name.
pub fn name_allows_default(q: &str, names: &[&str]) -> bool {
    let q = q.trim();
    if q.is_empty() {
        return false;
    }
    let long = q.chars().count() >= 4;
    names.iter().any(|n| {
        *n == q
            || (long
                && (n.starts_with(q)
                    || n.match_indices(' ').any(|(i, _)| n.get(i + 1..).is_some_and(|w| w.starts_with(q)))))
    })
}

/// The destructive rule of 5.2.7 for a result row and the current query: true when the row may be the default
/// selection. A row that is not destructive always may.
pub fn default_allowed(item: &Item, q: &Query) -> bool {
    if !item.destructive {
        return true;
    }
    let mut title = String::new();
    fold(&item.title, &mut title);
    if name_allows_default(&q.folded, &[&title]) {
        return true;
    }
    match &item.payload {
        Payload::Command(id) => name_allows_default(&q.folded, keywords::destructive_names(*id)),
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn folded(s: &str) -> String {
        let mut out = String::new();
        fold(s, &mut out);
        out
    }

    #[test]
    fn fold_removes_accents_and_case() {
        assert_eq!(folded("Attività"), "attivita");
        assert_eq!(folded("Gestione ATTIVITÀ"), "gestione attivita");
        assert_eq!(folded("Straße"), "strasse");
        assert_eq!(folded("Ærø Łódź Çñ Œ"), "aero lodz cn oe");
        assert_eq!(folded("Ångström"), "angstrom");
        assert_eq!(folded("ÀÈÌÒÙ àèìòù é"), "aeiou aeiou e");
    }

    #[test]
    fn fold_keeps_operators_and_other_scripts() {
        assert_eq!(folded("2 × 3 ÷ 4"), "2 × 3 ÷ 4");
        assert_eq!(folded("ΑΒΓ"), "αβγ");
        assert_eq!(folded("日本語"), "日本語");
        assert_eq!(folded(""), "");
    }

    #[test]
    fn fold_table_covers_the_range_with_ascii() {
        for cp in 0xC0u32..=0x17F {
            let c = char::from_u32(cp).unwrap();
            let f = folded(&c.to_string());
            if cp == 0xD7 || cp == 0xF7 {
                assert_eq!(f, c.to_string());
            } else {
                assert!(!f.is_empty() && f.chars().all(|x| x.is_ascii_lowercase()), "U+{cp:04X} -> {f:?}");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod fixtures {
    use super::*;
    use crate::launcher::keywords::BILINGUAL;
    use crate::launcher::model::{CommandId, GlyphId, IconRef, ItemKey, Kind, Mode, NumberStyle, ProviderId, VerbSet};

    /// (English name, Italian name, folded id for the bilingual table, Windows synonyms).
    type App = (&'static str, &'static str, &'static str, &'static [(&'static str, u32)]);

    const NONE: &[(&str, u32)] = &[];
    const APPS: &[App] = &[
        ("Calculator", "Calcolatrice", "microsoft.windowscalculator_8wekyb3d8bbwe!app", NONE),
        ("Task Manager", "Gestione attività", "taskmgr.exe", NONE),
        ("File Explorer", "Esplora file", "explorer.exe", NONE),
        ("Notepad", "Blocco note", "notepad.exe", NONE),
        ("Control Panel", "Pannello di controllo", "control.exe", NONE),
        ("Command Prompt", "Prompt dei comandi", "cmd.exe", NONE),
        (
            "Settings",
            "Impostazioni",
            "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
            NONE,
        ),
        ("Paint", "Paint", "mspaint.exe", NONE),
        ("Snipping Tool", "Strumento di cattura", "microsoft.screensketch_8wekyb3d8bbwe!app", NONE),
        ("Terminal", "Terminale", "microsoft.windowsterminal_8wekyb3d8bbwe!app", NONE),
        ("Windows PowerShell", "Windows PowerShell", "powershell.exe", NONE),
        ("Registry Editor", "Editor del Registro di sistema", "regedit.exe", NONE),
        ("Services", "Servizi", "services.msc", NONE),
        ("Device Manager", "Gestione dispositivi", "devmgmt.msc", NONE),
        ("Disk Management", "Gestione disco", "diskmgmt.msc", NONE),
        ("Event Viewer", "Visualizzatore eventi", "eventvwr.msc", NONE),
        ("Remote Desktop Connection", "Connessione Desktop remoto", "mstsc.exe", &[("mstsc", 2518), ("rdp", 2210)]),
        ("Character Map", "Mappa caratteri", "charmap.exe", NONE),
        ("Magnifier", "Lente di ingrandimento", "magnify.exe", NONE),
        ("On-Screen Keyboard", "Tastiera su schermo", "osk.exe", NONE),
        ("Photos", "Foto", "microsoft.windows.photos_8wekyb3d8bbwe!app", NONE),
        ("Camera", "Fotocamera", "microsoft.windowscamera_8wekyb3d8bbwe!app", NONE),
        ("Clock", "Orologio", "microsoft.windowsalarms_8wekyb3d8bbwe!app", NONE),
        ("Mail", "Posta", "microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.mail", NONE),
        (
            "Calendar",
            "Calendario",
            "microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.calendar",
            NONE,
        ),
        ("Maps", "Mappe", "microsoft.windowsmaps_8wekyb3d8bbwe!app", NONE),
        ("Weather", "Meteo", "microsoft.bingweather_8wekyb3d8bbwe!app", NONE),
        ("Media Player", "Lettore multimediale", "microsoft.zunemusic_8wekyb3d8bbwe!microsoft.zunemusic", NONE),
        ("Sound Recorder", "Registratore di suoni", "microsoft.windowssoundrecorder_8wekyb3d8bbwe!app", NONE),
        ("Sticky Notes", "Note adesive", "microsoft.microsoftstickynotes_8wekyb3d8bbwe!app", NONE),
        ("Task Scheduler", "Utilità di pianificazione", "taskschd.msc", NONE),
        ("System Information", "Informazioni di sistema", "msinfo32.exe", NONE),
        ("Resource Monitor", "Monitoraggio risorse", "resmon.exe", NONE),
        ("Performance Monitor", "Monitor prestazioni", "perfmon.exe", NONE),
        ("Windows Security", "Sicurezza di Windows", "microsoft.sechealthui_8wekyb3d8bbwe!sechealthui", NONE),
        ("Microsoft Store", "Microsoft Store", "microsoft.windowsstore_8wekyb3d8bbwe!app", NONE),
        ("Microsoft Edge", "Microsoft Edge", "msedge.exe", NONE),
        ("Xbox", "Xbox", "microsoft.gamingapp_8wekyb3d8bbwe!microsoft.xboxapp", NONE),
        ("Word", "Word", "winword.exe", NONE),
        ("Excel", "Excel", "excel.exe", NONE),
        ("Outlook", "Outlook", "outlook.exe", NONE),
        ("Teams", "Teams", "msteams_8wekyb3d8bbwe!msteams", NONE),
        ("Visual Studio Code", "Visual Studio Code", "code.exe", NONE),
        ("Steam", "Steam", "steam.exe", NONE),
        ("Google Chrome", "Google Chrome", "chrome.exe", NONE),
        ("Firefox", "Firefox", "firefox.exe", NONE),
        ("Discord", "Discord", "discord.exe", NONE),
        ("Spotify", "Spotify", "spotify.exe", NONE),
        ("7-Zip File Manager", "7-Zip File Manager", "7zfm.exe", NONE),
        ("Git Bash", "Git Bash", "git-bash.exe", NONE),
        ("GitHub Desktop", "GitHub Desktop", "githubdesktop.exe", NONE),
        ("Docker Desktop", "Docker Desktop", "docker desktop.exe", NONE),
        ("Adobe Photoshop 2025", "Adobe Photoshop 2025", "photoshop.exe", NONE),
        ("VLC media player", "VLC media player", "vlc.exe", NONE),
        ("Notepad++", "Notepad++", "notepad++.exe", NONE),
        ("Microsoft Visual Studio", "Microsoft Visual Studio", "devenv.exe", NONE),
        ("OneDrive", "OneDrive", "onedrive.exe", NONE),
    ];

    /// Commands: English name, Italian name, hidden keywords (commands.rs will own the real ones).
    const COMMANDS: &[(&str, &str, &[&str])] = &[
        ("Lock", "Blocca", &["lock", "blocca"]),
        ("Sleep", "Sospendi", &["sleep", "sospendi"]),
        ("Restart", "Riavvia", &["restart", "riavvia"]),
        ("Shut down", "Arresta", &["shut down", "arresta", "spegni"]),
        ("Sign out", "Disconnetti", &["sign out", "disconnetti"]),
        ("Window menu", "Menu finestra", &["window menu", "menu finestra", "sposta", "ridimensiona"]),
    ];

    struct Entry {
        en: &'static str,
        name: Indexed,
        keys: Keys,
        extra: &'static [&'static str],
        rank: u32,
        kind: u8,
    }

    fn pool(it: bool) -> Vec<Entry> {
        let mut v = Vec::new();
        for (i, (en, ita, id, keys)) in APPS.iter().enumerate() {
            v.push(Entry {
                en,
                name: Indexed::new(if it { ita } else { en }),
                keys: Keys::from(*keys),
                extra: keywords::bilingual_for(id),
                rank: i as u32,
                kind: 0,
            });
        }
        for (i, (en, ita, extra)) in COMMANDS.iter().enumerate() {
            v.push(Entry {
                en,
                name: Indexed::new(if it { ita } else { en }),
                keys: Keys::default(),
                extra,
                rank: 5000 + i as u32,
                kind: 2,
            });
        }
        v
    }

    fn fold_str(s: &str) -> String {
        let mut o = String::new();
        fold(s, &mut o);
        o
    }

    /// The caller's policy: tiers 0-5 over everything, tier 6 only when they found nothing. First by Score, then by
    /// folded name.
    fn best(it: bool, query: &str) -> Option<(&'static str, u8)> {
        let pool = pool(it);
        let cands: Vec<Cand> = pool.iter().map(|e| e.name.cand(&e.keys, e.extra, e.rank)).collect();
        let mut hits = Vec::new();
        match_all(&cands, &fold_str(query), &mut hits);
        hits.into_iter()
            .min_by(|&(a, ta), &(b, tb)| {
                let (ca, cb) = (&cands[a], &cands[b]);
                let sa = ca.score(ta, false, pool[a].kind);
                let sb = cb.score(tb, false, pool[b].kind);
                sa.cmp(&sb).then_with(|| ca.folded.cmp(cb.folded))
            })
            .map(|(i, t)| (pool[i].en, t))
    }

    const ROWS: &str = "
en|calculator|Calculator|0
en|notepad|Notepad|0
en|steam|Steam|0
en|paint|Paint|0
en|word|Word|0
en|excel|Excel|0
en|xbox|Xbox|0
en|outlook|Outlook|0
en|teams|Teams|0
en|photos|Photos|0
en|camera|Camera|0
en|clock|Clock|0
en|mail|Mail|0
en|maps|Maps|0
en|weather|Weather|0
en|services|Services|0
en|settings|Settings|0
en|terminal|Terminal|0
en|firefox|Firefox|0
en|discord|Discord|0
en|spotify|Spotify|0
en|onedrive|OneDrive|0
en|task manager|Task Manager|0
en|file explorer|File Explorer|0
en|control panel|Control Panel|0
en|notepad++|Notepad++|0
en|lock|Lock|0
en|sleep|Sleep|0
en|restart|Restart|0
en|shut down|Shut down|0
en|sign out|Sign out|0
en|window menu|Window menu|0
it|calcolatrice|Calculator|0
it|blocco note|Notepad|0
it|attività|Task Manager|2
it|gestione attività|Task Manager|0
it|esplora file|File Explorer|0
it|pannello di controllo|Control Panel|0
it|prompt dei comandi|Command Prompt|0
it|impostazioni|Settings|0
it|strumento di cattura|Snipping Tool|0
it|terminale|Terminal|0
it|servizi|Services|0
it|gestione dispositivi|Device Manager|0
it|gestione disco|Disk Management|0
it|visualizzatore eventi|Event Viewer|0
it|mappa caratteri|Character Map|0
it|lente di ingrandimento|Magnifier|0
it|tastiera su schermo|On-Screen Keyboard|0
it|foto|Photos|0
it|fotocamera|Camera|0
it|orologio|Clock|0
it|posta|Mail|0
it|calendario|Calendar|0
it|mappe|Maps|0
it|meteo|Weather|0
it|lettore multimediale|Media Player|0
it|registratore di suoni|Sound Recorder|0
it|note adesive|Sticky Notes|0
it|utilità di pianificazione|Task Scheduler|0
it|informazioni di sistema|System Information|0
it|monitoraggio risorse|Resource Monitor|0
it|monitor prestazioni|Performance Monitor|0
it|sicurezza di windows|Windows Security|0
it|arresta|Shut down|0
it|disconnetti|Sign out|0
it|sospendi|Sleep|0
it|riavvia|Restart|0
it|blocca|Lock|0
it|menu finestra|Window menu|0
en|calc|Calculator|1
en|task|Task Manager|1
en|file|File Explorer|1
en|control|Control Panel|1
en|comm|Command Prompt|1
en|set|Settings|1
en|snip|Snipping Tool|1
en|term|Terminal|1
en|regis|Registry Editor|1
en|serv|Services|1
en|dev|Device Manager|1
en|disk|Disk Management|1
en|event|Event Viewer|1
en|remote|Remote Desktop Connection|1
en|char|Character Map|1
en|magn|Magnifier|1
en|on-s|On-Screen Keyboard|1
en|pho|Photos|1
en|cam|Camera|1
en|clo|Clock|1
en|cal|Calculator|1
en|calen|Calendar|1
en|map|Maps|1
en|weat|Weather|1
en|media|Media Player|1
en|sound|Sound Recorder|1
en|sticky|Sticky Notes|1
en|system|System Information|1
en|resource|Resource Monitor|1
en|perf|Performance Monitor|1
en|microsoft e|Microsoft Edge|1
en|microsoft s|Microsoft Store|1
en|vis|Visual Studio Code|1
en|goo|Google Chrome|1
en|fire|Firefox|1
en|disc|Discord|1
en|spot|Spotify|1
en|ste|Steam|1
en|git|Git Bash|1
en|doc|Docker Desktop|1
en|vlc|VLC media player|1
en|7-z|7-Zip File Manager|1
en|one|OneDrive|1
en|ex|Excel|1
en|out|Outlook|1
en|tea|Teams|1
en|xb|Xbox|1
en|wor|Word|1
en|rest|Restart|1
en|shut|Shut down|1
en|sign|Sign out|1
en|sle|Sleep|1
en|window m|Window menu|1
en|task man|Task Manager|1
en|note|Notepad|1
en|adobe|Adobe Photoshop 2025|1
en|rdp|Remote Desktop Connection|1
en|mstsc|Remote Desktop Connection|1
en|regedit|Registry Editor|1
en|cmd|Command Prompt|1
en|taskmgr|Task Manager|1
en|defender|Windows Security|1
en|browser|Microsoft Edge|1
en|edge|Microsoft Edge|1
en|explorer|File Explorer|1
en|spegni|Shut down|1
en|riavvia|Restart|1
en|disconnetti|Sign out|1
en|sospendi|Sleep|1
en|blocca|Lock|1
en|menu finestra|Window menu|1
en|sposta|Window menu|1
en|blocco note|Notepad|1
en|esplora file|File Explorer|1
en|calcolatrice|Calculator|1
en|gestione attivita|Task Manager|1
en|pannello di controllo|Control Panel|1
en|impostazioni|Settings|1
en|orologio|Clock|1
en|fotocamera|Camera|1
en|foto|Photos|1
en|meteo|Weather|1
en|posta|Mail|1
en|mappe|Maps|1
en|servizi|Services|1
en|calendario|Calendar|1
it|spegni|Shut down|1
it|calc|Calculator|1
it|esplora|File Explorer|1
it|blocco|Notepad|1
it|pannello|Control Panel|1
it|calculator|Calculator|1
it|task manager|Task Manager|1
it|notepad|Notepad|1
it|settings|Settings|1
it|file explorer|File Explorer|1
it|command prompt|Command Prompt|1
it|control panel|Control Panel|1
it|shut down|Shut down|1
it|photos|Photos|1
it|weather|Weather|1
it|clock|Clock|1
it|mail|Mail|1
it|camera|Camera|1
it|calendar|Calendar|1
it|maps|Maps|1
it|remote desktop|Remote Desktop Connection|1
en|studio|Visual Studio Code|2
en|studio code|Visual Studio Code|2
en|code|Visual Studio Code|2
en|manager|Task Manager|2
en|monitor|Resource Monitor|2
en|powershell|Windows PowerShell|1
en|power|Windows PowerShell|2
en|shell|Windows PowerShell|2
en|drive|OneDrive|2
en|desktop|Remote Desktop Connection|2
en|bash|Git Bash|2
en|sched|Task Scheduler|2
en|viewer|Event Viewer|2
en|keyboard|On-Screen Keyboard|2
en|recorder|Sound Recorder|2
en|tool|Snipping Tool|2
en|prompt|Command Prompt|2
en|panel|Control Panel|2
en|player|Media Player|2
en|security|Windows Security|2
en|store|Microsoft Store|1
en|chrome|Google Chrome|2
en|photoshop|Adobe Photoshop 2025|2
en|2025|Adobe Photoshop 2025|2
en|notes|Sticky Notes|2
en|menu|Window menu|2
en|desk rem|Remote Desktop Connection|2
en|manager task|Task Manager|2
en|explorer file|File Explorer|2
it|dispositivi|Device Manager|2
it|disco|Discord|1
it|eventi|Event Viewer|2
it|controllo|Control Panel|2
it|registro|Registry Editor|2
it|suoni|Sound Recorder|2
it|schermo|On-Screen Keyboard|2
it|caratteri|Character Map|2
it|risorse|Resource Monitor|2
it|prestazioni|Performance Monitor|2
it|pianificazione|Task Scheduler|2
it|cattura|Snipping Tool|1
it|comandi|Command Prompt|2
it|sicurezza|Windows Security|1
it|remoto|Remote Desktop Connection|2
it|ingrandimento|Magnifier|2
it|adesive|Sticky Notes|2
it|multimediale|Media Player|2
en|vsc|Visual Studio Code|3
en|fe|File Explorer|3
en|tm|Task Manager|3
en|cp|Control Panel|3
en|gb|Git Bash|3
en|gc|Google Chrome|3
en|dd|Docker Desktop|3
en|wps|Windows PowerShell|3
en|rdc|Remote Desktop Connection|3
en|ghd|GitHub Desktop|3
en|vmp|VLC media player|3
en|mvs|Microsoft Visual Studio|3
en|ws|Windows Security|3
en|dm|Device Manager|3
it|gd|Device Manager|3
it|ga|Task Manager|3
it|rds|Registry Editor|3
en|eam|Teams|4
en|oogle|Google Chrome|4
en|culator|Calculator|4
en|xplor|File Explorer|4
en|hrome|Google Chrome|4
en|ntrol|Control Panel|4
en|oto|Photos|4
en|ager|Task Manager|4
en|ntrol pa|Control Panel|4
en|calcol|Calculator|5
en|esplora|File Explorer|5
en|blocco|Notepad|5
en|pannello|Control Panel|5
en|gestione|Task Manager|5
en|impost|Settings|5
en|fotoc|Camera|5
en|prompt dei|Command Prompt|5
en|strumento|Snipping Tool|5
en|lettore|Media Player|5
en|registratore|Sound Recorder|1
en|registrator|Sound Recorder|5
en|lente|Magnifier|5
en|tastiera|On-Screen Keyboard|5
en|mappa|Character Map|5
en|visualizzatore|Event Viewer|5
en|utilita|Task Scheduler|5
en|sicurezza|Windows Security|5
en|informazioni|System Information|5
en|monitoraggio|Resource Monitor|5
en|arre|Shut down|5
en|riav|Restart|5
en|sosp|Sleep|5
en|discon|Sign out|5
en|bloc|Notepad|5
it|snipping|Snipping Tool|5
it|sticky|Sticky Notes|5
en|clacolatrice|Calculator|6
en|notpead|Notepad|6
en|calclator|Calculator|6
en|firefx|Firefox|6
en|chorme|Google Chrome|6
en|spotfy|Spotify|6
en|discrod|Discord|6
en|exlorer|File Explorer|6
en|taks manager|Task Manager|6
en|steem|Steam|6
en|gogle|Google Chrome|6
it|notpad|Notepad|6
it|pannelo di controllo|Control Panel|6
it|calcolatrcie|Calculator|6
en|qqqqqq|-|0
en|xyz|-|0
en|hello world|-|0
en|lock screen|-|0
it|zzzz|-|0
";

    #[test]
    fn fixtures_150() {
        let mut n = 0;
        let mut bad = Vec::new();
        for line in ROWS.trim().lines() {
            let f: Vec<&str> = line.split('|').collect();
            let (it, q, want, tier) = (f[0] == "it", f[1], f[2], f[3].parse::<u8>().unwrap());
            n += 1;
            let got = best(it, q);
            let ok = if want == "-" { got.is_none() } else { got == Some((want, tier)) };
            if !ok {
                bad.push(format!("{line}  ->  {got:?}"));
            }
        }
        assert!(n >= 150, "only {n} rows");
        assert!(bad.is_empty(), "{} of {n} rows differ:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn fixtures_cover_every_tier() {
        for t in 0..=6 {
            assert!(ROWS.lines().filter(|l| l.ends_with(&format!("|{t}"))).count() >= 6, "tier {t}");
        }
    }

    #[test]
    fn tier6_only_when_nothing_else() {
        let pool = pool(false);
        let cands: Vec<Cand> = pool.iter().map(|e| e.name.cand(&e.keys, e.extra, e.rank)).collect();
        let mut out = Vec::new();
        // "notpead" matches nothing in tiers 0-5, so the fuzzy pass runs and every hit is tier 6.
        assert!(cands.iter().all(|c| match_tiers(c, "notpead").is_none()));
        match_all(&cands, "notpead", &mut out);
        assert!(!out.is_empty() && out.iter().all(|&(_, t)| t == 6));
        // "notepad" has tier 0-5 matches: no tier 6 row may appear next to them, though "notpad" is also close.
        match_all(&cands, "notepad", &mut out);
        assert!(!out.is_empty() && out.iter().all(|&(_, t)| t <= 5));
        // match_tiers itself never answers 6.
        for q in ["notpead", "clacolatrice", "xyz", "calc", "vsc", "bloc"] {
            assert!(cands.iter().all(|c| match_tiers(c, q).is_none_or(|t| t <= 5)));
        }
        // An empty query matches nothing, in either pass.
        assert!(cands.iter().all(|c| match_tiers(c, " ").is_none() && match_fuzzy(c, "").is_none()));
    }

    #[test]
    fn index_splits_words_and_builds_initials() {
        let x = Indexed::new("VisualStudio Code");
        assert_eq!(x.folded, "visualstudio code");
        assert_eq!(x.words, vec![0, 6, 13]);
        assert_eq!(x.initials, "vsc");
        let y = Indexed::new("Gestione attività (x64)");
        assert_eq!(y.folded, "gestione attivita (x64)");
        assert_eq!(y.words, vec![0, 9, 19]);
        assert_eq!(y.initials, "gax");
        let z = Indexed::new("PowerShell 7-preview");
        assert_eq!(z.initials, "ps7p");
        assert_eq!(Indexed::new("").words, Vec::<u16>::new());
        assert_eq!(Indexed::new("---").initials, "");
        // Folding can lengthen a name; offsets stay inside the folded text.
        let w = Indexed::new("Straße Ærø");
        assert_eq!(w.folded, "strasse aero");
        assert_eq!(w.words, vec![0, 8]);
    }

    #[test]
    fn osa_is_damerau_with_adjacent_swaps() {
        let d = |a: &str, b: &str| {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            osa(&a, &b)
        };
        assert_eq!(d("", ""), 0);
        assert_eq!(d("abc", "abc"), 0);
        assert_eq!(d("abc", "acb"), 1);
        assert_eq!(d("abc", "abd"), 1);
        assert_eq!(d("abc", "ab"), 1);
        assert_eq!(d("ab", "abc"), 1);
        assert_eq!(d("kitten", "sitting"), 3);
        assert_eq!(d("ca", "abc"), 3);
    }

    #[test]
    fn subsequence_allows_two_gaps_only() {
        let q = |s: &str| s.chars().collect::<Vec<char>>();
        assert!(subsequence_gaps("google chrome", &q("gogle"), 2));
        assert!(subsequence_gaps("visual studio code", &q("vstudio"), 2));
        assert!(subsequence_gaps("abcdef", &q("acdf"), 2));
        assert!(!subsequence_gaps("abcdefgh", &q("acegh"), 2));
        assert!(!subsequence_gaps("abc", &q("abcd"), 2));
        assert!(!subsequence_gaps("abc", &[], 2));
    }

    #[test]
    fn long_queries_do_not_overflow_the_fuzzy_buffers() {
        let x = Indexed::new("Calculator");
        let c = x.cand(Keys::none(), &[], 0);
        let long = "a".repeat(200);
        assert_eq!(match_fuzzy(&c, &long), None);
        assert_eq!(match_tiers(&c, &long), None);
    }

    #[test]
    fn bilingual_table_is_folded_and_findable() {
        assert!(BILINGUAL.len() >= 40);
        let mut seen = std::collections::HashSet::new();
        for &(key, words) in BILINGUAL.iter() {
            assert_eq!(fold_str(key), key, "key {key}");
            assert!(seen.insert(key), "duplicate {key}");
            assert!(!words.is_empty());
            for &w in words {
                assert_eq!(fold_str(w), w, "keyword {w}");
                assert_eq!(w.trim(), w);
            }
        }
        assert!(keywords::bilingual_for("microsoft.windowscalculator_8wekyb3d8bbwe!app").contains(&"calcolatrice"));
        assert!(keywords::bilingual_for("c:\\windows\\system32\\notepad.exe").contains(&"blocco note"));
        assert!(keywords::bilingual_for("msteams_8wekyb3d8bbwe!msteams").contains(&"teams"));
        assert!(keywords::bilingual_for("nothing.exe").is_empty());
        assert!(keywords::bilingual_for("").is_empty());
    }

    // ------------------------------------------------------------ the destructive rule

    fn query(folded: &str) -> Query<'static> {
        Query {
            raw: "",
            text: "",
            folded: folded.to_string(),
            mode: Mode::Mixed,
            gen: 1,
            num: NumberStyle { decimal: '.', group: ',' },
            chip: None,
            apps_also: false,
        }
    }

    fn command(title: &str, id: CommandId, destructive: bool) -> Item {
        Item {
            key: ItemKey::of(ProviderId::Commands, title),
            from: ProviderId::Commands,
            kind: Kind::Command,
            title: title.into(),
            detail: None,
            icon: IconRef::Glyph(GlyphId::Power),
            score: Score { tier: 1, learned: Reverse(0), idle: true, kind: 2, rank: 0, len: 4 },
            verbs: VerbSet::empty(),
            payload: Payload::Command(id),
            destructive,
        }
    }

    fn destructive_items() -> Vec<(Item, &'static str)> {
        vec![
            (command("Restart", CommandId::Restart, true), "restart"),
            (command("Shut down", CommandId::ShutDown, true), "shut down"),
            (command("Sign out", CommandId::SignOut, true), "sign out"),
            (command("Sleep", CommandId::Sleep, true), "sleep"),
            // The Italian display names.
            (command("Riavvia", CommandId::Restart, true), "riavvia"),
            (command("Arresta", CommandId::ShutDown, true), "arresta"),
            (command("Disconnetti", CommandId::SignOut, true), "disconnetti"),
            (command("Sospendi", CommandId::Sleep, true), "sospendi"),
        ]
    }

    #[test]
    fn destructive_never_default_for_short_queries() {
        for (item, _) in destructive_items() {
            for q in ["s", "sh", "re", "spe", "sl", "si", "sig", "shu", "sle", "ri", "riav"] {
                // "riav" and the 4-character prefixes are checked below; the short ones never select.
                if q.chars().count() < 4 {
                    assert!(!default_allowed(&item, &query(q)), "{} with {q:?}", item.title);
                }
            }
            assert!(!default_allowed(&item, &query("")), "{} with an empty query", item.title);
            assert!(!default_allowed(&item, &query("xyz")), "{} with {:?}", item.title, "xyz");
        }
    }

    #[test]
    fn destructive_default_for_names_and_long_prefixes() {
        let allowed = |title: &str, id: CommandId, q: &str| default_allowed(&command(title, id, true), &query(q));
        for q in ["shut", "shut d", "shut down", "shutdown", "spegni", "arre", "arresta", "down"] {
            assert!(allowed("Shut down", CommandId::ShutDown, q), "shut down {q:?}");
        }
        for q in ["sleep", "slee", "sosp", "sospendi", "sospensione"] {
            assert!(allowed("Sleep", CommandId::Sleep, q), "sleep {q:?}");
        }
        for q in ["riav", "riavvia", "restart", "rest", "reboot"] {
            assert!(allowed("Restart", CommandId::Restart, q), "restart {q:?}");
        }
        for q in ["sign", "sign out", "disc", "disconnetti", "logoff", "log"] {
            // "log" is three characters and not a name: only the first five count.
            assert_eq!(allowed("Sign out", CommandId::SignOut, q), q != "log", "sign out {q:?}");
        }
        // "power" and "off" belong to no destructive name (the Power options page must not default to Shut down).
        for q in ["power", "power off", "off", "powe"] {
            assert!(!allowed("Shut down", CommandId::ShutDown, q), "shut down {q:?}");
        }
        // Not another command's words.
        assert!(!allowed("Sleep", CommandId::Sleep, "shut"));
        assert!(!allowed("Restart", CommandId::Restart, "sleep"));
        // The Italian title alone is enough, with no command table behind it.
        let mut it = command("Spegni il PC", CommandId::ShutDown, true);
        it.payload = Payload::None;
        assert!(default_allowed(&it, &query("spegni")));
        assert!(!default_allowed(&it, &query("spe")));
    }

    #[test]
    fn non_destructive_rows_are_always_allowed() {
        let lock = command("Lock", CommandId::Lock, false);
        for q in ["", "l", "xyz", "lock"] {
            assert!(default_allowed(&lock, &query(q)));
        }
    }
}
