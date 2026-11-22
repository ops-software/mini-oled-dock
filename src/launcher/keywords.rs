//! The names of the inbox apps in both languages (SPEC 5.2.5, plan L3 T5): AppsFolder names come in the Windows display
//! language, so a compiled table gives every inbox app its English and Italian names, which match like its shown name,
//! and a few short synonyms, which are hidden words. Every string is folded (lowercase, no accents). Entries are found
//! by folded AUMID, package family name or exe name, and failing that by the folded shown name. Commands carry their
//! own names in commands.rs (L1a-7).

use super::model::CommandId;

/// One inbox program: the ids it can have, its names (English and Italian, folded; matched like the shown name), its
/// hidden synonyms (matched like Windows' synonyms) and the classic programs whose popularity rank it inherits when
/// its own id has none.
pub struct Inbox {
    pub ids: &'static [&'static str],
    pub names: &'static [&'static str],
    pub synonyms: &'static [&'static str],
    pub classic: &'static [&'static str],
}

macro_rules! inbox {
    ([$($id:expr),*], [$($n:expr),*], [$($s:expr),*], [$($c:expr),*]) => {
        Inbox { ids: &[$($id),*], names: &[$($n),*], synonyms: &[$($s),*], classic: &[$($c),*] }
    };
}

pub static INBOX: &[Inbox] = &[
    inbox!(["microsoft.windowscalculator", "calc.exe"], ["calculator", "calcolatrice", "calc"], [], []),
    inbox!(["taskmgr.exe"], ["task manager", "gestione attivita", "taskmgr"], [], []),
    inbox!(["explorer.exe", "microsoft.windows.explorer"], ["file explorer", "esplora file"], [], []),
    inbox!(
        ["notepad.exe", "microsoft.windowsnotepad"],
        ["notepad", "blocco note"],
        [],
        ["{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}\\notepad.exe"]
    ),
    inbox!(["control.exe", "microsoft.windows.controlpanel"], ["control panel", "pannello di controllo"], [], []),
    inbox!(["cmd.exe"], ["command prompt", "prompt dei comandi", "cmd"], [], []),
    inbox!(
        ["windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel", "systemsettings.exe"],
        ["settings", "impostazioni"],
        [],
        []
    ),
    inbox!(["mspaint.exe", "microsoft.paint"], ["paint"], [], ["{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}\\mspaint.exe"]),
    inbox!(
        ["microsoft.screensketch", "snippingtool.exe"],
        ["snipping tool", "strumento di cattura"],
        [],
        ["{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}\\snippingtool.exe"]
    ),
    inbox!(["microsoft.windowsterminal"], ["terminal", "windows terminal", "terminale"], [], []),
    inbox!(["powershell.exe"], ["windows powershell", "powershell"], [], []),
    inbox!(
        ["regedit.exe"],
        ["registry editor", "editor del registro di sistema", "editor del registro", "regedit"],
        [],
        []
    ),
    inbox!(["services.msc"], ["services", "servizi"], [], []),
    inbox!(["devmgmt.msc"], ["device manager", "gestione dispositivi"], [], []),
    inbox!(["diskmgmt.msc"], ["disk management", "gestione disco"], [], []),
    inbox!(["eventvwr.msc"], ["event viewer", "visualizzatore eventi"], [], []),
    inbox!(
        ["mstsc.exe", "microsoft.windows.remotedesktop"],
        ["remote desktop connection", "connessione desktop remoto", "remote desktop", "desktop remoto", "mstsc"],
        ["rdp"],
        []
    ),
    inbox!(["charmap.exe"], ["character map", "mappa caratteri"], [], []),
    inbox!(["magnify.exe"], ["magnifier", "lente di ingrandimento"], [], []),
    inbox!(["osk.exe"], ["on-screen keyboard", "tastiera su schermo"], [], []),
    inbox!(["microsoft.windows.photos"], ["photos", "foto"], [], []),
    inbox!(["microsoft.windowscamera"], ["camera", "fotocamera"], [], []),
    inbox!(
        ["microsoft.windowsalarms"],
        ["clock", "orologio", "alarms & clock", "sveglie e orologio"],
        ["alarms", "sveglie"],
        []
    ),
    inbox!(["microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.mail"], ["mail", "posta"], [], []),
    inbox!(
        ["microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.calendar"],
        ["calendar", "calendario"],
        [],
        []
    ),
    inbox!(["microsoft.windowsmaps"], ["maps", "mappe"], [], []),
    inbox!(["microsoft.bingweather"], ["weather", "meteo"], [], []),
    inbox!(["microsoft.zunemusic"], ["media player", "lettore multimediale"], [], []),
    inbox!(["microsoft.windowssoundrecorder"], ["sound recorder", "registratore di suoni"], [], []),
    inbox!(["microsoft.microsoftstickynotes"], ["sticky notes", "memo", "note adesive"], [], []),
    inbox!(["taskschd.msc"], ["task scheduler", "utilita di pianificazione"], [], []),
    inbox!(["msinfo32.exe"], ["system information", "informazioni di sistema", "msinfo32"], [], []),
    inbox!(["resmon.exe"], ["resource monitor", "monitoraggio risorse", "resmon"], [], []),
    inbox!(
        ["perfmon.exe"],
        ["performance monitor", "monitoraggio prestazioni", "monitor prestazioni", "perfmon"],
        [],
        []
    ),
    inbox!(["microsoft.sechealthui"], ["windows security", "sicurezza di windows"], ["defender"], []),
    inbox!(["microsoft.windowsstore"], ["microsoft store"], ["store"], []),
    inbox!(["msedge.exe", "msedge"], ["microsoft edge"], ["browser"], []),
    inbox!(["microsoft.gamingapp"], ["xbox"], [], []),
    inbox!(["winword.exe"], ["word", "microsoft word"], [], []),
    inbox!(["excel.exe"], ["excel", "microsoft excel"], [], []),
    inbox!(["outlook.exe"], ["outlook", "microsoft outlook"], [], []),
    inbox!(["ms-teams.exe", "msteams"], ["teams", "microsoft teams"], [], []),
    inbox!(["compmgmt.msc"], ["computer management", "gestione computer"], [], []),
    inbox!(["msconfig.exe"], ["system configuration", "configurazione di sistema", "msconfig"], [], []),
    inbox!(["cleanmgr.exe"], ["disk cleanup", "pulizia disco", "cleanmgr"], [], []),
    inbox!(["microsoft.windows.shell.rundialog"], ["run", "esegui"], [], []),
    inbox!(["microsoft.windows.administrativetools"], ["windows tools", "strumenti di windows"], [], []),
    inbox!(["narrator.exe"], ["narrator", "assistente vocale"], [], []),
    inbox!(["microsoftcorporationii.quickassist"], ["quick assist", "assistenza rapida"], [], []),
    inbox!(["dfrgui.exe"], ["defragment and optimize drives", "deframmenta e ottimizza unita"], ["defrag"], []),
    inbox!(["mdsched.exe"], ["windows memory diagnostic", "diagnostica memoria windows"], [], []),
    inbox!(["recoverydrive.exe"], ["recovery drive", "unita di ripristino"], [], []),
    inbox!(["comexp.msc"], ["component services", "servizi componenti"], [], []),
    inbox!(["secpol.msc"], ["local security policy", "criteri di sicurezza locali", "secpol"], [], []),
    inbox!(["voiceaccess.exe"], ["voice access", "accesso vocale"], [], []),
    inbox!(["livecaptions.exe"], ["live captions", "sottotitoli in tempo reale"], [], []),
    inbox!(["printmanagement.msc"], ["print management", "gestione stampa"], [], []),
    inbox!(["microsoft.commandpalette"], ["command palette", "riquadro comandi"], [], []),
];

/// The inbox entry of an app and its index in `INBOX` (what the arena stores): by the whole folded id, by its package
/// family (the part before the first "_"), by its file name (after the last separator), then by the folded shown name.
/// Allocates nothing.
pub fn inbox_for(folded_id: &str, folded_name: &str) -> Option<(u16, &'static Inbox)> {
    let family = folded_id.split('_').next().unwrap_or(folded_id);
    let file = folded_id.rsplit(['\\', '/']).next().unwrap_or(folded_id);
    let at = INBOX
        .iter()
        .position(|e| e.ids.iter().any(|k| *k == folded_id || *k == family || *k == file))
        .or_else(|| INBOX.iter().position(|e| e.names.contains(&folded_name)))?;
    Some((u16::try_from(at).ok()?, INBOX.get(at)?))
}

/// Names and aliases of the destructive commands (5.2.7), folded, English and Italian. The first is the display name.
pub fn destructive_names(id: CommandId) -> &'static [&'static str] {
    match id {
        CommandId::Sleep => &["sleep", "sospendi", "sospensione", "standby"],
        CommandId::Restart => &["restart", "riavvia", "riavvio", "reboot"],
        CommandId::ShutDown => &["shut down", "shutdown", "arresta", "arresta il sistema", "spegni", "spegnimento"],
        CommandId::SignOut => &["sign out", "signout", "log off", "logoff", "disconnetti", "esci dall'account"],
        _ => &[],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::launcher::rank::fold;

    fn folded(s: &str) -> String {
        let mut out = String::new();
        fold(s, &mut out);
        out
    }

    #[test]
    fn inbox_found_by_id_family_file_or_name() {
        let first_name = |id: &str, name: &str| inbox_for(id, name).map(|(_, e)| e.names[0]);
        // By the package family (the part before the first underscore).
        assert_eq!(first_name("microsoft.windowsnotepad_8wekyb3d8bbwe!app", ""), Some("notepad"));
        // By the file name of a path.
        assert_eq!(first_name("c:\\windows\\system32\\taskmgr.exe", ""), Some("task manager"));
        // By the whole id.
        assert_eq!(first_name("calc.exe", ""), Some("calculator"));
        // An AutoGenerated id has nothing to find: the shown name does.
        let guid = "microsoft.autogenerated.{0f0e1a2b-0000-0000-0000-000000000001}";
        assert_eq!(first_name(guid, "gestione attivita"), Some("task manager"));
        // Unknown id and name.
        assert_eq!(first_name(guid, "invented tool"), None);
        assert_eq!(first_name("", ""), None);
        // The index is the entry's place in the table.
        let (at, e) = inbox_for("msteams_8wekyb3d8bbwe!msteams", "").unwrap();
        assert!(std::ptr::eq(&INBOX[usize::from(at)], e));
    }

    #[test]
    fn inbox_table_is_folded_and_unique() {
        assert!(INBOX.len() >= 50);
        let mut seen = std::collections::HashSet::new();
        for e in INBOX {
            assert!(!e.ids.is_empty() && !e.names.is_empty());
            for id in e.ids.iter().chain(e.classic) {
                assert_eq!(folded(id), *id, "id {id}");
            }
            for w in e.names.iter().chain(e.synonyms) {
                assert_eq!(folded(w), *w, "name {w}");
                assert_eq!(w.trim(), *w);
            }
            for id in e.ids {
                assert!(seen.insert(*id), "{id} is in two entries");
            }
        }
    }
}
