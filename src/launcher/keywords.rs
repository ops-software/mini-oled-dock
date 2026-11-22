//! The other language of the inbox apps (SPEC 5.2.5): AppsFolder names come in the Windows display language, so a
//! compiled table adds the English and Italian names as hidden keywords. Every string is folded (lowercase, no
//! accents) and the table is keyed by folded AUMID, package family name or exe name. Commands carry their own
//! keywords in commands.rs (L1a-7).

use super::model::CommandId;

/// Folded id (full AUMID, package family name before the underscore, or exe or console-tool file name) and the hidden
/// keywords of that app.
pub static BILINGUAL: [(&str, &[&str]); 44] = [
    ("microsoft.windowscalculator", &["calculator", "calcolatrice", "calc"]),
    ("calc.exe", &["calculator", "calcolatrice", "calc"]),
    ("taskmgr.exe", &["task manager", "gestione attivita", "taskmgr"]),
    ("explorer.exe", &["file explorer", "esplora file", "explorer"]),
    ("notepad.exe", &["notepad", "blocco note"]),
    ("control.exe", &["control panel", "pannello di controllo"]),
    ("cmd.exe", &["command prompt", "prompt dei comandi", "cmd"]),
    (
        "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
        &["settings", "impostazioni"],
    ),
    ("systemsettings.exe", &["settings", "impostazioni"]),
    ("mspaint.exe", &["paint"]),
    ("microsoft.screensketch", &["snipping tool", "strumento di cattura", "cattura"]),
    ("snippingtool.exe", &["snipping tool", "strumento di cattura", "cattura"]),
    ("microsoft.windowsterminal", &["terminal", "terminale"]),
    ("powershell.exe", &["powershell", "terminal"]),
    ("regedit.exe", &["registry editor", "editor del registro", "regedit"]),
    ("services.msc", &["services", "servizi"]),
    ("devmgmt.msc", &["device manager", "gestione dispositivi"]),
    ("diskmgmt.msc", &["disk management", "gestione disco"]),
    ("eventvwr.msc", &["event viewer", "visualizzatore eventi"]),
    ("mstsc.exe", &["remote desktop", "desktop remoto", "rdp"]),
    ("charmap.exe", &["character map", "mappa caratteri"]),
    ("magnify.exe", &["magnifier", "lente di ingrandimento"]),
    ("osk.exe", &["on-screen keyboard", "tastiera su schermo"]),
    ("microsoft.windows.photos", &["photos", "foto"]),
    ("microsoft.windowscamera", &["camera", "fotocamera"]),
    ("microsoft.windowsalarms", &["clock", "orologio", "alarms", "sveglie"]),
    ("microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.mail", &["mail", "posta"]),
    ("microsoft.windowscommunicationsapps_8wekyb3d8bbwe!microsoft.windowslive.calendar", &["calendar", "calendario"]),
    ("microsoft.windowsmaps", &["maps", "mappe"]),
    ("microsoft.bingweather", &["weather", "meteo"]),
    ("microsoft.zunemusic", &["media player", "lettore multimediale"]),
    ("microsoft.windowssoundrecorder", &["sound recorder", "registratore di suoni", "registratore"]),
    ("microsoft.microsoftstickynotes", &["sticky notes", "note adesive"]),
    ("taskschd.msc", &["task scheduler", "utilita di pianificazione", "pianificazione attivita"]),
    ("msinfo32.exe", &["system information", "informazioni di sistema"]),
    ("resmon.exe", &["resource monitor", "monitoraggio risorse"]),
    ("perfmon.exe", &["performance monitor", "monitoraggio prestazioni", "monitor prestazioni"]),
    ("microsoft.sechealthui", &["windows security", "sicurezza di windows", "defender"]),
    ("microsoft.windowsstore", &["microsoft store", "store"]),
    ("msedge.exe", &["edge", "microsoft edge", "browser"]),
    ("microsoft.gamingapp", &["xbox"]),
    ("winword.exe", &["word", "microsoft word"]),
    ("excel.exe", &["excel", "microsoft excel"]),
    ("outlook.exe", &["outlook", "microsoft outlook"]),
];

/// Teams has two ids over the years (classic exe and the new package); both stay in one place.
const TEAMS: (&[&str], &[&str]) = (&["ms-teams.exe", "msteams"], &["teams", "microsoft teams"]);

/// The hidden keywords of an app, from its folded AUMID, package family name or exe name; empty when it has none.
/// `id` is tried whole, then up to its first "_", then after its last path separator. Allocates nothing.
pub fn bilingual_for(id: &str) -> &'static [&'static str] {
    let family = id.split('_').next().unwrap_or(id);
    let file = id.rsplit(['\\', '/']).next().unwrap_or(id);
    for probe in [id, family, file] {
        if let Some((_, words)) = BILINGUAL.iter().find(|(key, _)| *key == probe) {
            return words;
        }
        if TEAMS.0.contains(&probe) {
            return TEAMS.1;
        }
    }
    &[]
}

/// Names and aliases of the destructive commands (5.2.7), folded, English and Italian. The first is the display name.
pub fn destructive_names(id: CommandId) -> &'static [&'static str] {
    match id {
        CommandId::Sleep => &["sleep", "sospendi", "sospensione", "standby"],
        CommandId::Restart => &["restart", "riavvia", "riavvio", "reboot"],
        CommandId::ShutDown => {
            &["shut down", "shutdown", "arresta", "arresta il sistema", "spegni", "spegnimento", "power off"]
        }
        CommandId::SignOut => &["sign out", "signout", "log off", "logoff", "disconnetti", "esci dall'account"],
        _ => &[],
    }
}
