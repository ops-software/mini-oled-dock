//! Run lines (5.4.9): classification by syntax only, the known-command table and the suggestions of the run mode.
//! Nothing here reads the environment, the file system or the registry, and nothing expands %VAR%: the line goes to
//! Explorer as typed.

use super::model::RunLine;
use super::rank::fold;
use crate::i18n::tr;

/// True once live check 4 shows that Explorer expands %VAR% in a line it is given. While it is true the console
/// fallback below never applies; set it to false to make such lines run in a console kept open instead.
pub const EXPLORER_EXPANDS_VARS: bool = true;

/// One entry of the known-command table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Known {
    /// Folded file name without ".exe".
    pub name: &'static str,
    pub en: &'static str,
    pub it: &'static str,
    /// A console program: it runs in a console that stays open.
    pub console: bool,
}

impl Known {
    pub fn description(&self) -> &'static str {
        tr(self.en, self.it)
    }
}

const fn k(name: &'static str, en: &'static str, it: &'static str, console: bool) -> Known {
    Known { name, en, it, console }
}

pub static KNOWN: [Known; 51] = [
    k("winget", "Windows package manager", "Gestione pacchetti di Windows", true),
    k("ipconfig", "Show network addresses", "Mostra gli indirizzi di rete", true),
    k("ping", "Test a network connection", "Verifica una connessione di rete", true),
    k("tracert", "Trace the route to a host", "Traccia il percorso verso un host", true),
    k("pathping", "Trace a route and measure loss", "Traccia il percorso e misura le perdite", true),
    k("netstat", "Show network connections", "Mostra le connessioni di rete", true),
    k("nslookup", "Query a DNS name", "Interroga un nome DNS", true),
    k("netsh", "Network configuration", "Configurazione di rete", true),
    k("getmac", "Show MAC addresses", "Mostra gli indirizzi MAC", true),
    k("arp", "Show the ARP cache", "Mostra la cache ARP", true),
    k("route", "Show the routing table", "Mostra la tabella di routing", true),
    k("sfc", "Check system files", "Controlla i file di sistema", true),
    k("dism", "Repair the Windows image", "Ripara l'immagine di Windows", true),
    k("chkdsk", "Check a disk", "Controlla un disco", true),
    k("systeminfo", "Show system information", "Mostra le informazioni di sistema", true),
    k("whoami", "Show the current user", "Mostra l'utente corrente", true),
    k("tasklist", "List running processes", "Elenca i processi in esecuzione", true),
    k("powercfg", "Power settings and reports", "Impostazioni e report di alimentazione", true),
    k("gpupdate", "Refresh group policy", "Aggiorna i criteri di gruppo", true),
    k("wsl", "Windows Subsystem for Linux", "Sottosistema Windows per Linux", true),
    k("notepad", "Text editor", "Editor di testo", false),
    k("calc", "Calculator", "Calcolatrice", false),
    k("mspaint", "Paint", "Paint", false),
    k("cmd", "Command Prompt", "Prompt dei comandi", false),
    k("powershell", "Windows PowerShell", "Windows PowerShell", false),
    k("pwsh", "PowerShell 7", "PowerShell 7", false),
    k("wt", "Windows Terminal", "Terminale Windows", false),
    k("explorer", "File Explorer", "Esplora file", false),
    k("regedit", "Registry Editor", "Editor del Registro di sistema", false),
    k("mstsc", "Remote Desktop", "Connessione desktop remoto", false),
    k("msconfig", "System Configuration", "Configurazione di sistema", false),
    k("services.msc", "Services", "Servizi", false),
    k("devmgmt.msc", "Device Manager", "Gestione dispositivi", false),
    k("diskmgmt.msc", "Disk Management", "Gestione disco", false),
    k("compmgmt.msc", "Computer Management", "Gestione computer", false),
    k("eventvwr", "Event Viewer", "Visualizzatore eventi", false),
    k("taskmgr", "Task Manager", "Gestione attività", false),
    k("control", "Control Panel", "Pannello di controllo", false),
    k("appwiz.cpl", "Programs and Features", "Programmi e funzionalità", false),
    k("ncpa.cpl", "Network Connections", "Connessioni di rete", false),
    k("sysdm.cpl", "System Properties", "Proprietà del sistema", false),
    k("mmc", "Management Console", "Console di gestione", false),
    k("osk", "On-Screen Keyboard", "Tastiera su schermo", false),
    k("magnify", "Magnifier", "Lente di ingrandimento", false),
    k("charmap", "Character Map", "Mappa caratteri", false),
    k("snippingtool", "Snipping Tool", "Strumento di cattura", false),
    k("winver", "About Windows", "Informazioni su Windows", false),
    k("dxdiag", "DirectX diagnostics", "Diagnostica DirectX", false),
    k("msinfo32", "System Information", "Informazioni di sistema", false),
    k("resmon", "Resource Monitor", "Monitoraggio risorse", false),
    k("perfmon", "Performance Monitor", "Monitor delle prestazioni", false),
];

/// The table entry for a file name or path: its last component, folded, without ".exe".
pub fn known_for(file: &str) -> Option<&'static Known> {
    let name = file.rsplit(['\\', '/']).next().unwrap_or(file);
    let mut folded = String::with_capacity(name.len());
    fold(name, &mut folded);
    let base = folded.strip_suffix(".exe").unwrap_or(&folded);
    KNOWN.iter().find(|e| e.name == base)
}

fn has_prefix_ci(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

/// A URL scheme followed by "://" at the start of the text.
fn is_url(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3
        && b.first().is_some_and(u8::is_ascii_alphabetic)
        && b.get(1) == Some(&b':')
        && b.get(2) == Some(&b'\\')
}

/// Splits a typed line by syntax alone. None for an empty line (or an empty quoted file).
pub fn classify(line: &str) -> Option<RunLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let whole = |console: bool| RunLine { line: line.into(), file: 0..line.len(), console };
    if let Some(rest) = line.strip_prefix('"') {
        let len = rest.find('"').unwrap_or(rest.len());
        if len == 0 {
            return None;
        }
        let file = 1..1 + len;
        let console = known_for(rest.get(..len)?).is_some_and(|e| e.console);
        return Some(RunLine { line: line.into(), file, console });
    }
    if is_url(line) || has_prefix_ci(line, "shell:") || has_prefix_ci(line, "ms-settings:") {
        return Some(whole(false));
    }
    if is_drive_path(line) || line.starts_with("\\\\") {
        return Some(whole(known_for(line).is_some_and(|e| e.console)));
    }
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    let console = known_for(line.get(..end)?).is_some_and(|e| e.console);
    Some(RunLine { line: line.into(), file: 0..end, console })
}

/// "%" followed later by "%": the shape of an environment variable.
pub fn needs_console_for_vars(line: &str) -> bool {
    line.find('%').is_some_and(|i| line.get(i + 1..).is_some_and(|rest| rest.contains('%')))
}

/// Whether the line runs in a console kept open: a known console program, or (only while Explorer is not known to
/// expand variables) a line with a %VAR%.
pub fn runs_in_console(r: &RunLine) -> bool {
    r.console || (!EXPLORER_EXPANDS_VARS && needs_console_for_vars(&r.line))
}

/// Where a suggestion came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Typed,
    History,
    Known(&'static Known),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Suggestion {
    pub text: Box<str>,
    pub source: Source,
}

const MAX_SUGGESTIONS: usize = 8;

/// The suggestions of the run mode, in run.rs's order: the typed line, history lines that start with it, known
/// commands that start with it, history lines that contain it. At most 8, no line twice. `history` is newest first.
/// Tab completes from these only.
pub fn suggest(text: &str, history: &[Box<str>], out: &mut Vec<Suggestion>) {
    out.clear();
    let typed = text.trim();
    let mut want = String::with_capacity(typed.len());
    fold(typed, &mut want);
    let mut seen: Vec<String> = Vec::with_capacity(MAX_SUGGESTIONS);
    let mut add = |out: &mut Vec<Suggestion>, shown: &str, folded: String, source: Source| {
        if out.len() < MAX_SUGGESTIONS && !seen.contains(&folded) {
            seen.push(folded);
            out.push(Suggestion { text: shown.into(), source });
        }
    };
    if !typed.is_empty() {
        add(out, typed, want.clone(), Source::Typed);
    }
    let mut folded_history: Vec<String> = Vec::with_capacity(history.len());
    for h in history {
        let mut f = String::with_capacity(h.len());
        fold(h, &mut f);
        folded_history.push(f);
    }
    for (h, f) in history.iter().zip(&folded_history) {
        if f.starts_with(&want) {
            add(out, h, f.clone(), Source::History);
        }
    }
    if !want.contains(char::is_whitespace) {
        for e in &KNOWN {
            if e.name.starts_with(&want) {
                add(out, e.name, e.name.to_string(), Source::Known(e));
            }
        }
    }
    for (h, f) in history.iter().zip(&folded_history) {
        if f.contains(&want) {
            add(out, h, f.clone(), Source::History);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::testkit::XorShift;

    fn parts(line: &str) -> Option<(String, String, bool)> {
        classify(line).map(|r| (r.line.to_string(), r.line[r.file.clone()].to_string(), r.console))
    }

    fn file_of(line: &str) -> Option<String> {
        parts(line).map(|p| p.1)
    }

    #[test]
    fn classification() {
        // The first word is the file, the rest the arguments.
        assert_eq!(parts("notepad c:\\a b.txt"), Some(("notepad c:\\a b.txt".into(), "notepad".into(), false)));
        // A quoted first part is the file, quotes excluded.
        assert_eq!(
            parts("\"C:\\Program Files\\x.exe\" -y"),
            Some(("\"C:\\Program Files\\x.exe\" -y".into(), "C:\\Program Files\\x.exe".into(), false))
        );
        // Trimmed.
        assert_eq!(parts("  ping  1.1.1.1 "), Some(("ping  1.1.1.1".into(), "ping".into(), true)));
        // Taken whole: URL, shell:, ms-settings:, drive path, UNC.
        assert_eq!(file_of("https://example.com/a b"), Some("https://example.com/a b".into()));
        assert_eq!(file_of("shell:startup"), Some("shell:startup".into()));
        assert_eq!(file_of("SHELL:Startup"), Some("SHELL:Startup".into()));
        assert_eq!(file_of("ms-settings:display"), Some("ms-settings:display".into()));
        assert_eq!(file_of("C:\\Program Files\\x.exe -y"), Some("C:\\Program Files\\x.exe -y".into()));
        assert_eq!(file_of("\\\\server\\share\\a b.txt"), Some("\\\\server\\share\\a b.txt".into()));
        // A drive letter without a backslash is an ordinary word.
        assert_eq!(file_of("c: x"), Some("c:".into()));
        // Nothing to run.
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
        assert_eq!(classify("\"\""), None);
        assert_eq!(classify("\" x"), Some(RunLine { line: "\" x".into(), file: 1..3, console: false }));
        // An unterminated quote takes the rest as the file.
        assert_eq!(file_of("\"abc def"), Some("abc def".into()));
        // No environment expansion: the line stays as typed.
        assert_eq!(parts("%windir%\\notepad.exe").map(|p| p.0), Some("%windir%\\notepad.exe".into()));
        // A URL is never a console program.
        assert!(!classify("https://x/ping").unwrap().console);
    }

    #[test]
    fn known_console_programs() {
        assert_eq!(KNOWN.len(), 51);
        assert_eq!(KNOWN.iter().filter(|e| e.console).count(), 20);
        for name in ["winget", "ipconfig", "ping", "wsl", "PING", "ping.exe", "Ping.EXE"] {
            assert!(classify(name).unwrap().console, "{name}");
        }
        for name in ["notepad", "cmd", "powershell", "wt", "services.msc", "explorer.exe", "unknown"] {
            assert!(!classify(name).unwrap().console, "{name}");
        }
        assert!(classify("C:\\Windows\\System32\\ping.exe 1.1.1.1 -t").is_none_or(|r| !r.console));
        assert!(classify("\"C:\\Windows\\System32\\ping.exe\" 1.1.1.1").unwrap().console);
        assert!(classify("winget search git").unwrap().console);
        for e in &KNOWN {
            assert_eq!(e.name, e.name.to_lowercase(), "{}", e.name);
            assert!(!e.en.is_empty() && !e.it.is_empty());
            assert!(!e.description().is_empty());
        }
        let mut names: Vec<&str> = KNOWN.iter().map(|e| e.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), KNOWN.len());
    }

    #[test]
    fn variables_fallback_is_selectable() {
        assert!(needs_console_for_vars("echo %PATH%"));
        assert!(needs_console_for_vars("a%b c%"));
        assert!(!needs_console_for_vars("100%"));
        assert!(!needs_console_for_vars("%"));
        // While Explorer is assumed to expand, a variable line is not a console line.
        assert!(!runs_in_console(&classify("%windir%\\notepad.exe").unwrap()));
    }

    #[test]
    fn suggestions_come_in_run_rs_order() {
        let history: Vec<Box<str>> =
            ["ping 8.8.8.8", "notepad a.txt", "winget upgrade", "ping -t pi"].iter().map(|s| Box::from(*s)).collect();
        let mut out = Vec::new();
        suggest("pi", &history, &mut out);
        let texts: Vec<&str> = out.iter().map(|s| &*s.text).collect();
        assert_eq!(texts, ["pi", "ping 8.8.8.8", "ping -t pi", "ping"]);
        assert_eq!(out[0].source, Source::Typed);
        assert_eq!(out[1].source, Source::History);
        assert!(matches!(out[3].source, Source::Known(e) if e.name == "ping"));
        // A substring match comes last.
        suggest("txt", &history, &mut out);
        let texts: Vec<&str> = out.iter().map(|s| &*s.text).collect();
        assert_eq!(texts, ["txt", "notepad a.txt"]);
        assert_eq!(out[1].source, Source::History);
        // At most 8, and nothing twice.
        suggest("", &history, &mut out);
        assert_eq!(out.len(), 8);
        let mut texts: Vec<&str> = out.iter().map(|s| &*s.text).collect();
        texts.sort_unstable();
        texts.dedup();
        assert_eq!(texts.len(), 8);
        // A typed line that is also in the history is listed once.
        suggest("ping 8.8.8.8", &history, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn seeded_100k_never_panics() {
        let mut r = XorShift::new(0x2B1E);
        let history: Vec<Box<str>> = ["ping 1.1.1.1", "C:\\x y.exe"].iter().map(|s| Box::from(*s)).collect();
        let mut out = Vec::new();
        let atoms =
            ["\"", " ", "\\", "\\\\", "C:", ":", "https://", "shell:", "ms-settings:", "%", "ping", ".exe", "é"];
        for i in 0..100_000 {
            let t = if i % 2 == 0 {
                r.ascii_or_bmp(40)
            } else {
                let n = r.next_u64() % 8;
                (0..n).map(|_| *r.pick(&atoms)).collect::<String>()
            };
            if let Some(rl) = classify(&t) {
                assert!(rl.file.end <= rl.line.len() && rl.line.is_char_boundary(rl.file.start));
                assert!(rl.line.is_char_boundary(rl.file.end));
                let _ = runs_in_console(&rl);
            }
            suggest(&t, &history, &mut out);
            assert!(out.len() <= 8);
        }
    }
}
