//! PF1: one line per frozen item of the plumbing plan (F-P5, F-P6) with its full signature, so a change fails to compile.
//! The component table and the pure constants are real and are checked here.
// Function-pointer types spelled out in full are the point of this file.
#![allow(clippy::type_complexity)]

use oled_trust::journal::*;
use oled_trust::manifest::release::Version;
use oled_trust::manifest::suite::*;

#[test]
fn journal_items_keep_their_signatures() {
    let _: [usize; 4] = [JOURNAL_SCHEMA as usize, MAX_LINE, MAX_RECORDS, SNAPSHOT_MAX];
    let _: [usize; 2] = [TASK_XML_MAX, SNAPSHOT_DEPTH_MAX as usize];
    let _a: fn(&str) -> Result<RegPath, JournalError> = RegPath::new;
    let _b: fn(&RegPath) -> &str = RegPath::as_str;
    let _c: fn(&str) -> Result<TaskName, JournalError> = TaskName::new;
    let _d: fn(&TaskName) -> &str = TaskName::as_str;
    let _e: fn(Scope) -> Journal = Journal::empty;
    let _f: fn(Scope, &[u8]) -> Result<Journal, JournalError> = Journal::parse;
    let _g: fn(&Record) -> Result<Vec<u8>, JournalError> = Journal::encode;
    let _h: fn(&Journal) -> &[Record] = Journal::records;
    let _i: for<'a> fn(&'a Journal) -> Vec<&'a Entry> = Journal::open_entries;
    let _j: fn(&Journal) -> bool = Journal::is_settled;
    let _k: fn(&mut Journal) -> RunId = Journal::begin_run;
    let _l: fn(&Journal) -> u64 = Journal::next_seq;
    let _m: fn(&RegSnapshot) -> Vec<u8> = canonical_tree;
    let _n: fn(&RegSnapshot, &dyn JournalOs) -> Result<Digest, OsErr> = digest_tree;
    let _o: fn(&str, &dyn JournalOs) -> Result<Digest, OsErr> = digest_text;
    let _p: fn(&mut Journal, &mut dyn Sink, RunId, Kind, Change, u64) -> Result<u64, OsErr> = record;
    let _q: fn(&mut Journal, &mut dyn Sink, RunId) -> Result<(), OsErr> = commit;
    let _r: fn(&mut Journal, &mut dyn Sink, &mut dyn JournalOs, Plan, u64) -> Result<RunId, ApplyError> = apply;
    let _s: fn(&mut Journal, &mut dyn Sink, &mut dyn JournalOs, &UndoSet) -> UndoReport = undo;
    let _t: fn(&Journal) -> Vec<EntryView> = views;
    // The enums and records keep their variants and fields.
    let _ = (Hive::Hkcu, Hive::Hklm, TakeoverId::FolderVerbs, AppId::Dock, AppId::Tasks, AppId::Files);
    let _ =
        (KindClass::Takeover, KindClass::Install, KindClass::Update, KindClass::UserEdit, Scope::User, Scope::Machine);
    let _ = Kind::Takeover(TakeoverId::FolderVerbs);
    let _ = (Kind::Install, Kind::Update, Kind::UserEdit(AppId::Dock), Kind::Unknown(String::new()));
    let _ = RegData::Other { ty: 0, bytes: Vec::new() };
    let _: fn(RegPath) -> Change =
        |key| Change::RegValue { hive: Hive::Hkcu, key, name: None, before: None, after: None };
    let _ = Change::Unknown(String::new());
    let _ = ApplyError::TooLarge(0);
    let _ = UndoSet::Seqs(Vec::new());
    let _ = UndoReport::default();
    let _ = Target::File(String::new());
    let _ = Record::Undone { seq: 0 };
    let _ = JournalError::Version(0);
}

#[test]
fn suite_items_keep_their_signatures() {
    let _: (&str, u64, u16, u64, [&str; 2], &str) =
        (SUITE_ASSET, SUITE_MAX_BYTES, SUITE_SCHEMA, COMPONENT_MAX_BYTES, SUITE_FILES, LANG_DIR);
    let _a: fn(&[u8]) -> Result<SuiteManifest, String> = parse_manifest;
    let _b: fn(&SuiteManifest) -> String = encode_manifest;
    let _c: fn(&SuiteManifest, &[OnDisk], &[(ComponentId, [u8; 32])], bool) -> Vec<Step> = plan_convergence;
    let _d: fn(ComponentId) -> [String; 4] = ComponentId::install_names;
    let _e: fn(&str) -> Option<ComponentId> = ComponentId::parse;
    let _ = Component {
        id: ComponentId::Dock,
        version: Version::default(),
        sha256: [0; 32],
        size: 0,
        stage: Stage::Stable,
    };
    let _ = SuiteManifest { schema: SUITE_SCHEMA, version: Version::default(), components: Vec::new() };
    let _ = OnDisk { id: ComponentId::Dock, current: None, prev: None, staged: None };
    let _ = (Source::Stage, Source::Prev, Source::Download, Stage::Beta);
    let _ = (
        Step::Keep(ComponentId::Dock),
        Step::Add(ComponentId::Tasks, Source::Stage),
        Step::Replace(ComponentId::Files, Source::Prev),
        Step::Skip(ComponentId::LangIt),
    );
}

#[test]
fn component_names() {
    use ComponentId::*;
    assert_eq!(ComponentId::ALL, [Dock, Tasks, TasksAdmin, Files, LangIt]);
    let table = [
        (Dock, "dock", "dock.exe", "dock.prev.exe", "dock.exe"),
        (Tasks, "tasks", "tasks.exe", "tasks.prev.exe", "tasks.exe"),
        (TasksAdmin, "tasks-admin", "tasks-admin.exe", "tasks-admin.prev.exe", "tasks-admin.exe"),
        (Files, "files", "files.exe", "files.prev.exe", "files.exe"),
        (LangIt, "lang-it", "lang\\it.lang", "lang\\it.prev.lang", "lang-it.lang"),
    ];
    for (c, id, file, prev, asset) in table {
        assert_eq!((c.id(), c.file(), c.prev(), c.asset()), (id, file, prev, asset));
        assert_eq!(ComponentId::parse(id), Some(c));
        assert_eq!(
            c.install_names(),
            [file.to_string(), format!("{file}.new"), prev.to_string(), format!("{prev}.new")]
        );
    }
    assert_eq!(ComponentId::parse("x"), None);
    assert_eq!(ComponentId::parse(""), None);
    assert_eq!(ComponentId::parse("Dock"), None);
    assert_eq!(SUITE_FILES, ["suite.json", "suite.json.tmp"]);
    assert_eq!(LANG_DIR, "lang");
}
