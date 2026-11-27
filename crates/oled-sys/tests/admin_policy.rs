//! TA2: the tasks-admin refusal policy in pure form (the R3 classification and R4 policy rows of SPEC-tasks).

use oled_sys::admin::policy::*;
use oled_trust::frame::admin::{
    AdminRequest, DenyReason, KeyList, MachineStartupId, PriorityClass, ProcKey, ServiceName, StartupSource, SvcOp,
    PROTO,
};

fn key(pid: u32, created: u64) -> ProcKey {
    ProcKey { pid, created }
}

/// Every policy, in the column order of the admit table: mode, Administrator protection, command line enabled.
const POLICIES: [(Mode, bool, bool); 8] = [
    (Mode::Session, false, false),
    (Mode::Session, false, true),
    (Mode::Session, true, false),
    (Mode::Session, true, true),
    (Mode::OneShot, false, false),
    (Mode::OneShot, false, true),
    (Mode::OneShot, true, false),
    (Mode::OneShot, true, true),
];

#[test]
fn admit_table() {
    use DenyReason::{BadFrame, Disabled, NeedsConsent};
    let k = key(1234, 5678);
    let ok = Ok(());
    // Process verbs: refused only by a session under Administrator protection (no session exists under AP).
    let plain = [ok, ok, Err(Disabled), Err(Disabled), ok, ok, ok, ok];
    // Consent verbs: a session asks for consent per action, a one-shot helper is that consent.
    let consent = [Err(NeedsConsent), Err(NeedsConsent), Err(Disabled), Err(Disabled), ok, ok, ok, ok];
    // The command line: only when enabled and never under Administrator protection.
    let cmdline = [Err(Disabled), ok, Err(Disabled), Err(Disabled), Err(Disabled), ok, Err(Disabled), Err(Disabled)];
    let item = MachineStartupId { source: StartupSource::HklmRun, ordinal: 3, name_hash: 0xdead_beef };
    let rows: Vec<(AdminRequest, [Result<(), DenyReason>; 8])> = vec![
        (AdminRequest::Hello { proto: PROTO }, plain),
        (AdminRequest::Hello { proto: PROTO + 1 }, [Err(BadFrame); 8]),
        (AdminRequest::Hello { proto: 0 }, [Err(BadFrame); 8]),
        (AdminRequest::Hello { proto: u16::MAX }, [Err(BadFrame); 8]),
        (AdminRequest::Terminate { keys: KeyList::new(vec![k]).unwrap(), tree: false }, plain),
        (AdminRequest::Terminate { keys: KeyList::new(vec![k, key(1, 2)]).unwrap(), tree: true }, plain),
        (AdminRequest::SetPriority { key: k, class: PriorityClass::Idle }, plain),
        (AdminRequest::SetPriority { key: k, class: PriorityClass::High }, plain),
        (AdminRequest::SetEfficiency { key: k, on: true }, plain),
        (AdminRequest::SetEfficiency { key: k, on: false }, plain),
        (AdminRequest::QueryCommandLine { key: k }, cmdline),
        (AdminRequest::ServiceControl { name: ServiceName::new("Spooler").unwrap(), op: SvcOp::Start }, consent),
        (AdminRequest::ServiceControl { name: ServiceName::new("Spooler").unwrap(), op: SvcOp::Stop }, consent),
        (AdminRequest::ServiceControl { name: ServiceName::new("Spooler").unwrap(), op: SvcOp::Restart }, consent),
        (AdminRequest::StartupSetEnabled { item, on: true }, consent),
        (AdminRequest::StartupSetEnabled { item, on: false }, consent),
        (AdminRequest::RestartDock, consent),
    ];
    for (req, want) in &rows {
        for (i, &(mode, admin_protection, command_line_enabled)) in POLICIES.iter().enumerate() {
            let p = Policy { mode, admin_protection, command_line_enabled };
            assert_eq!(admit_request(req, &p), want[i], "{req:?} under {p:?}");
        }
    }
}

const ME: u32 = 900;
const SESSION: u32 = 1;
const DOCK: ProcKey = ProcKey { pid: 700, created: 40 };
const WATCHDOG: ProcKey = ProcKey { pid: 701, created: 41 };
const GUARD: ProcKey = ProcKey { pid: 702, created: 42 };
const UPDATE_GUARD: ProcKey = ProcKey { pid: 703, created: 43 };
const PARENT: ProcKey = ProcKey { pid: 800, created: 50 };

fn helper() -> HelperFacts {
    HelperFacts {
        self_pid: ME,
        parent: PARENT,
        session: SESSION,
        dock_family: [Some(DOCK), Some(WATCHDOG), Some(GUARD), Some(UPDATE_GUARD)],
    }
}

/// A plain live process in the helper's session that every rule admits.
fn plain(pid: u32, created: u64) -> TargetFacts {
    TargetFacts {
        key: key(pid, created),
        live_created: Some(created),
        session: SESSION,
        image_name: "app.exe".into(),
        has_image_path: true,
        under_system_root: false,
        critical_flag: Some(false),
        protected: false,
        hosts_reboot_service: false,
    }
}

/// The real lsass: no image path visible to a query and flagged critical.
fn lsass(session: u32) -> TargetFacts {
    TargetFacts {
        image_name: "lsass.exe".into(),
        has_image_path: false,
        critical_flag: Some(true),
        session,
        ..plain(640, 30)
    }
}

const CHANGING: [TargetVerb; 3] = [TargetVerb::Terminate, TargetVerb::SetPriority, TargetVerb::SetEfficiency];

#[test]
fn target_order_and_each_reason() {
    use DenyReason::*;
    let h = helper();
    let mut all = CHANGING.to_vec();
    all.push(TargetVerb::QueryCommandLine);
    for &verb in &all {
        assert_eq!(check_target(verb, &plain(1500, 99), &h), Ok(()), "{verb:?}");
        // (1) Gone: the key no longer names the live process, or its create time is unknown.
        assert_eq!(check_target(verb, &TargetFacts { live_created: Some(100), ..plain(1500, 99) }, &h), Err(Gone));
        assert_eq!(check_target(verb, &TargetFacts { live_created: None, ..plain(1500, 99) }, &h), Err(Gone));
        // (3) Another session, or session 0.
        assert_eq!(check_target(verb, &TargetFacts { session: 0, ..plain(1500, 99) }, &h), Err(OtherSession));
        assert_eq!(check_target(verb, &TargetFacts { session: 2, ..plain(1500, 99) }, &h), Err(OtherSession));
    }
    for &verb in &CHANGING {
        // (2) The helper itself, its parent, the idle and system processes and every key of the dock family.
        for t in [plain(0, 0), plain(4, 0), plain(ME, 77), plain(PARENT.pid, PARENT.created)] {
            assert_eq!(check_target(verb, &t, &h), Err(OwnProcess), "{t:?}");
        }
        for k in [DOCK, WATCHDOG, GUARD, UPDATE_GUARD] {
            assert_eq!(check_target(verb, &plain(k.pid, k.created), &h), Err(OwnProcess), "{k:?}");
        }
        // A process that took over the parent's pid later is not the parent.
        assert_eq!(check_target(verb, &plain(PARENT.pid, PARENT.created + 1), &h), Ok(()));
        assert_eq!(check_target(verb, &plain(DOCK.pid, DOCK.created + 1), &h), Ok(()));
        // (4) Critical by name (no image path) or by the critical flag.
        assert_eq!(check_target(verb, &lsass(SESSION), &h), Err(Critical));
        assert_eq!(
            check_target(verb, &TargetFacts { critical_flag: Some(true), ..plain(1500, 99) }, &h),
            Err(Critical)
        );
        let named = TargetFacts { image_name: "WinLogon.EXE".into(), critical_flag: None, ..plain(1500, 99) };
        assert_eq!(check_target(verb, &TargetFacts { has_image_path: false, ..named.clone() }, &h), Err(Critical));
        assert_eq!(check_target(verb, &TargetFacts { under_system_root: true, ..named.clone() }, &h), Err(Critical));
        // An unknown critical flag on an ordinary name is not critical.
        assert_eq!(check_target(verb, &TargetFacts { critical_flag: None, ..plain(1500, 99) }, &h), Ok(()));
        // (5) A process that hosts a service whose failure action reboots the machine.
        let reboot = TargetFacts { hosts_reboot_service: true, ..plain(1500, 99) };
        assert_eq!(check_target(verb, &reboot, &h), Err(RebootOnFailure));
        // (6) A protected process.
        assert_eq!(check_target(verb, &TargetFacts { protected: true, ..plain(1500, 99) }, &h), Err(Protected));

        // The order.
        let gone_critical = TargetFacts { live_created: None, ..lsass(SESSION) };
        assert_eq!(check_target(verb, &gone_critical, &h), Err(Gone));
        assert_eq!(check_target(verb, &lsass(SESSION), &h), Err(Critical));
        assert_eq!(check_target(verb, &lsass(0), &h), Err(OtherSession));
        assert_eq!(check_target(verb, &TargetFacts { live_created: None, ..plain(ME, 77) }, &h), Err(Gone));
        assert_eq!(check_target(verb, &TargetFacts { session: 0, ..plain(4, 0) }, &h), Err(OwnProcess));
        let everything = TargetFacts { hosts_reboot_service: true, protected: true, ..lsass(SESSION) };
        assert_eq!(check_target(verb, &everything, &h), Err(Critical));
        let reboot_protected = TargetFacts { protected: true, ..reboot.clone() };
        assert_eq!(check_target(verb, &reboot_protected, &h), Err(RebootOnFailure));
    }
    // QueryCommandLine applies only (1) and (3): it reads, it changes nothing.
    let q = TargetVerb::QueryCommandLine;
    assert_eq!(check_target(q, &lsass(SESSION), &h), Ok(()));
    assert_eq!(check_target(q, &plain(ME, 77), &h), Ok(()));
    assert_eq!(check_target(q, &plain(DOCK.pid, DOCK.created), &h), Ok(()));
    assert_eq!(
        check_target(q, &TargetFacts { protected: true, hosts_reboot_service: true, ..plain(1500, 99) }, &h),
        Ok(())
    );
    assert_eq!(check_target(q, &lsass(0), &h), Err(OtherSession));
    assert_eq!(check_target(q, &TargetFacts { live_created: Some(31), ..lsass(SESSION) }, &h), Err(Gone));
}

#[test]
fn restart_dock_only_on_the_dock() {
    use DenyReason::*;
    let r = TargetVerb::RestartDock;
    let h = helper();
    let dock = plain(DOCK.pid, DOCK.created);
    assert_eq!(check_target(r, &dock, &h), Ok(()));
    // Protected (6) is a rule of the three process verbs only.
    assert_eq!(check_target(r, &TargetFacts { protected: true, ..dock.clone() }, &h), Ok(()));
    // The dock's own pid with another create time is another process.
    assert_eq!(check_target(r, &plain(DOCK.pid, DOCK.created + 1), &h), Err(OwnProcess));
    // Everything else is refused as OwnProcess: the rest of the family, the helper, its parent, any other process.
    for k in [WATCHDOG, GUARD, UPDATE_GUARD, PARENT, key(ME, 77), key(4, 0), key(0, 0), key(1500, 99)] {
        assert_eq!(check_target(r, &plain(k.pid, k.created), &h), Err(OwnProcess), "{k:?}");
    }
    // No dock known: nothing can be restarted.
    let none = HelperFacts { dock_family: [None, Some(WATCHDOG), Some(GUARD), Some(UPDATE_GUARD)], ..h };
    assert_eq!(check_target(r, &dock, &none), Err(OwnProcess));
    // The rules before and after (2) still hold for the dock.
    assert_eq!(check_target(r, &TargetFacts { live_created: None, ..dock.clone() }, &h), Err(Gone));
    assert_eq!(check_target(r, &TargetFacts { session: 2, ..dock.clone() }, &h), Err(OtherSession));
    assert_eq!(check_target(r, &TargetFacts { critical_flag: Some(true), ..dock.clone() }, &h), Err(Critical));
    assert_eq!(check_target(r, &TargetFacts { hosts_reboot_service: true, ..dock.clone() }, &h), Err(RebootOnFailure));
    // The dock is never a target of the process verbs.
    for &verb in &CHANGING {
        assert_eq!(check_target(verb, &dock, &h), Err(OwnProcess));
    }
}

#[test]
fn fake_csrss_is_not_critical() {
    // A copy in a download folder is not protected by its name.
    assert!(!is_critical_image("csrss.exe", true, false));
    // The real one: no path visible, or a path under SystemRoot.
    assert!(is_critical_image("csrss.exe", false, false));
    assert!(is_critical_image("csrss.exe", true, true));
    assert!(is_critical_image("CSRSS.EXE", true, true));
    for name in CRITICAL_IMAGES {
        assert!(is_critical_image(name, false, false), "{name}");
        assert!(is_critical_image(&name.to_ascii_uppercase(), true, true), "{name}");
        assert!(is_critical_image(&name.to_ascii_lowercase(), false, true), "{name}");
        assert!(!is_critical_image(name, true, false), "{name}");
    }
    for other in ["csrss", "csrss.exe.exe", "xcsrss.exe", "csrss.exe ", "Systém", "System32", "", "lsass.ex\u{0130}"] {
        assert!(!is_critical_image(other, false, true), "{other:?}");
    }
    let h = helper();
    let fake = TargetFacts { image_name: "csrss.exe".into(), critical_flag: None, ..plain(1500, 99) };
    assert_eq!(check_target(TargetVerb::Terminate, &fake, &h), Ok(()));
    let real = TargetFacts { has_image_path: false, ..fake.clone() };
    assert_eq!(check_target(TargetVerb::Terminate, &real, &h), Err(DenyReason::Critical));
}

fn edge(pid: u32, created: u64, parent_pid: u32) -> ProcEdge {
    ProcEdge { key: key(pid, created), parent_pid }
}

#[test]
fn descendants_respect_creation_time() {
    let root = key(100, 1000);
    let procs = [
        edge(100, 1000, 50),  // the root itself
        edge(150, 800, 100),  // a child of an older process with pid 100: not adopted
        edge(200, 1500, 100), // child
        edge(300, 1700, 200), // grandchild, listed before the second child
        edge(201, 1600, 100), // child
        edge(301, 1400, 200), // older than 200: the child of an earlier pid 200, not adopted
        edge(302, 1650, 201), // grandchild
        edge(400, 1800, 300), // great-grandchild
        edge(500, 1000, 100), // created at the same tick as the root: not later, not adopted
        edge(200, 1500, 100), // the same key twice: once
        edge(600, 2000, 999), // unrelated
    ];
    let mut out = vec![key(9, 9), key(8, 8)];
    descendants(root, &procs, &mut out);
    let want = [root, key(200, 1500), key(201, 1600), key(300, 1700), key(302, 1650), key(400, 1800)];
    assert_eq!(out, want, "root first, breadth first, each key once");

    // The R2 helper tree test in pure form: pid 100 recycled by a later process adopts none of the old tree.
    descendants(key(100, 9000), &procs, &mut out);
    assert_eq!(out, [key(100, 9000)]);
    // A root missing from the list is still the first key; a leaf has no descendants.
    descendants(key(77, 1), &[], &mut out);
    assert_eq!(out, [key(77, 1)]);
    descendants(key(400, 1800), &procs, &mut out);
    assert_eq!(out, [key(400, 1800)]);
    // A process listed as its own parent adopts nothing and the walk ends.
    descendants(key(5, 10), &[edge(5, 10, 5), edge(6, 11, 5), edge(6, 11, 6)], &mut out);
    assert_eq!(out, [key(5, 10), key(6, 11)]);
}

#[test]
fn service_paths() {
    let roots = [r"C:\Windows", r"C:\Program Files", r"C:\Program Files (x86)"];
    let ok = [
        r#""C:\Program Files\Vendor App\svc.exe" -k"#,
        r"C:\Windows\System32\svchost.exe -k netsvcs",
        r"c:\windows\system32\x.exe",
        r"C:\WINDOWS\SYSTEM32\SVCHOST.EXE -k netsvcs",
        // The spaces are all in the arguments.
        r"C:\Windows\System32\svchost.exe -k netsvcs -p",
        // The only space of the program is inside the root.
        r"C:\Program Files\Vendor\svc.exe --mode service",
        r"C:\Program Files (x86)\Vendor\svc.exe -x",
        r#""C:\Program Files\Vendor App\svc.exe" -k a b"#,
        r#""C:\Program Files (x86)\Vendor App\svc.exe""#,
        r#""C:\Windows\System32\svchost.exe"-k"#,
        // No ".exe" and nothing after it: Windows appends ".exe" to this one name.
        r"C:\Windows\System32\svchost",
    ];
    for path in ok {
        assert_eq!(service_image_allowed(path, &roots), Ok(()), "{path}");
    }
    assert_eq!(service_image_allowed(r"c:\windows\system32\x.exe", &[r"C:\Windows"]), Ok(()));
    assert_eq!(service_image_allowed(r"C:\Windows\x.exe", &[r"C:\Windows\"]), Ok(()), "a root's last backslash");
    let refused = [
        // Unquoted with a space after the root: Windows tries C:\Program Files\Vendor.exe first.
        r"C:\Program Files\Vendor App\svc.exe",
        r"C:\Program Files\Vendor App\svc.exe -run",
        r"C:\Windows\System32\svc host.exe",
        // Outside every root.
        r"C:\Users\Public\svc.exe",
        r#""C:\Users\Public\svc.exe" -k"#,
        r"D:\Windows\x.exe",
        r"C:\WindowsApps\x.exe",
        r"C:\Windows",
        r"C:\Windows\",
        // Not drive-absolute.
        r"\\?\C:\Windows\x.exe",
        r"\\.\C:\Windows\x.exe",
        r"\\server\share\x.exe",
        r"Windows\x.exe",
        r"x.exe",
        r"%SystemRoot%\System32\svchost.exe -k netsvcs",
        r" C:\Windows\x.exe",
        "",
        r#""""#,
        // Escapes and odd forms inside a root.
        r"C:\Windows\..\Temp\x.exe",
        r#""C:\Windows\..\Users\Public\x.exe""#,
        r"C:\Windows\x\\?\C:\y.exe",
        r"C:\Windows\x\\.\y.exe",
        r"C:/Windows/x.exe",
        r"C:\Windows/x.exe",
        r"C:\Windows\x.exe:stream",
        r#""C:\Windows\x.exe:stream" -k"#,
        "C:\\Windows\\x\ty.exe",
        r#"C:\Windows\x"y.exe"#,
        // An unclosed quote.
        r#""C:\Windows\x.exe -k"#,
        // No ".exe" and arguments: when the first name is missing Windows also tries the longer prefixes.
        r"C:\Windows\a ..\..\..\Users\Public\evil -k",
        r"C:\Windows\System32\svchost -k netsvcs",
    ];
    for path in refused {
        assert_eq!(service_image_allowed(path, &roots), Err(DenyReason::ServiceImagePath), "{path}");
    }
    assert_eq!(service_image_allowed(r"C:\Windows\x.exe", &[]), Err(DenyReason::ServiceImagePath));
    // A root must name a folder: a drive alone or a relative root admits nothing.
    for root in [r"C:\", "C:", "", r"Windows", r"\Windows"] {
        assert_eq!(service_image_allowed(r"C:\Windows\x.exe", &[root]), Err(DenyReason::ServiceImagePath), "{root:?}");
    }
}

#[test]
fn unquoted_arguments_cannot_climb_out_of_the_programs_folder() {
    // When the program of an unquoted line is missing, CreateProcess goes on to the longer space-separated prefixes
    // (the program, a space and part of the arguments, ".exe" appended) and normalises each one: a ".." segment there
    // climbs out of the program's folder. `C:\Windows\x.exe \..\..\Users\Public\evil` runs C:\Users\Public\evil.exe.
    let roots = [r"C:\Windows", r"C:\Program Files"];
    let refused = [
        r"C:\Windows\x.exe \..\..\Users\Public\evil",
        r"C:\Windows\x.exe /../../Users/Public/evil",
        r"C:\Windows\System32\x.exe -c ..\..\..\..\Users\Public\evil",
        r"C:\Program Files\Vendor\svc.exe --config \..\..\..\Users\Public\evil -k",
        r"C:\Windows\x.exe a\..\..\..\Temp\evil",
    ];
    for path in refused {
        assert_eq!(service_image_allowed(path, &roots), Err(DenyReason::ServiceImagePath), "{path}");
    }
    // A quoted program is the only name Windows tries. Without "..", every longer prefix lies in a folder named after
    // the program, inside the program's own folder, so slashes, colons and backslashes in the arguments are harmless.
    let ok = [
        r#""C:\Windows\x.exe" \..\..\Users\Public\evil"#,
        r"C:\Windows\System32\dllhost.exe /Processid:{02D4B3F1-FD88-11D1-960D-00805FC79235}",
        r"C:\Windows\System32\msiexec.exe /V",
        r"C:\Windows\System32\x.exe -config C:\ProgramData\Vendor\x.ini",
    ];
    for path in ok {
        assert_eq!(service_image_allowed(path, &roots), Ok(()), "{path}");
    }
}

#[test]
fn folders_a_standard_user_can_write_inside_a_root_are_refused() {
    // SystemRoot holds folders where a standard user can create files or folders (Temp, the task folders, tracing, the
    // colour profiles, ...): a program planted there lies under the root, but the user chose it.
    let roots = [r"C:\Windows", r"C:\Program Files"];
    let refused = [
        r"C:\Windows\Temp\x.exe",
        r"c:\windows\temp\x.exe -k",
        r#""C:\Windows\Temp\Vendor Setup\x.exe" -k"#,
        r"C:\Windows\Tasks\x.exe",
        r"C:\Windows\tracing\x.exe",
        r"C:\Windows\Registration\CRMLog\x.exe",
        r"C:\Windows\debug\WIA\x.exe",
        r"C:\Windows\PLA\Reports\x.exe",
        r"C:\Windows\System32\Tasks\Microsoft\Windows\SyncCenter\x.exe",
        r"C:\Windows\System32\Tasks_Migrated\x.exe",
        r"C:\Windows\System32\spool\drivers\color\x.exe",
        r"C:\Windows\System32\spool\PRINTERS\x.exe",
        r"C:\Windows\System32\com\dmp\x.exe",
        r"C:\Windows\System32\FxsTmp\x.exe",
        r"C:\Windows\System32\LogFiles\WMI\x.exe",
        r"C:\Windows\System32\Microsoft\Crypto\RSA\MachineKeys\x.exe",
        r"C:\Windows\SysWOW64\Tasks\x.exe",
        r"C:\Windows\SysWOW64\com\dmp\x.exe",
        r"C:\Windows\SysWOW64\FxsTmp\x.exe",
        // The same folders as Windows normalises them: an empty or "." segment, a trailing dot or space.
        r#""C:\Windows\\Temp\x.exe""#,
        r#""C:\Windows\.\Temp\x.exe""#,
        r#""C:\Windows\Temp.\x.exe""#,
        r#""C:\Windows\Temp \x.exe""#,
        r#""C:\Windows\System32\\spool\.\drivers\color\x.exe""#,
        // Below every root, not only SystemRoot.
        r"C:\Program Files\Temp\x.exe",
    ];
    for path in refused {
        assert_eq!(service_image_allowed(path, &roots), Err(DenyReason::ServiceImagePath), "{path}");
    }
    // Their neighbours stay admitted.
    let ok = [
        r"C:\Windows\System32\spoolsv.exe",
        r"C:\Windows\System32\Tasks.exe",
        r"C:\Windows\System32\com\x.exe",
        r"C:\Windows\System32\Microsoft\Crypto\x.exe",
        r"C:\Windows\System32\svchost.exe -k netsvcs",
        r#""C:\Program Files\Vendor App\svc.exe" -k"#,
    ];
    for path in ok {
        assert_eq!(service_image_allowed(path, &roots), Ok(()), "{path}");
    }
}

#[test]
fn service_stop_refuses_a_reboot_on_failure() {
    assert_eq!(service_stop_allowed(true), Err(DenyReason::RebootOnFailure));
    assert_eq!(service_stop_allowed(false), Ok(()));
}

#[test]
fn rate_20_per_second() {
    let mut r = RateLimiter::new();
    for t in 0..20 {
        assert!(r.admit(t), "admit {t}");
    }
    assert!(!r.admit(19));
    assert!(!r.admit(999), "the oldest (0) is 999 ms old");
    assert!(r.admit(1000), "the oldest (0) is 1000 ms old");
    assert!(!r.admit(1000), "the oldest is now 1");
    assert!(r.admit(1001));
    // A refusal is not an admit: a flood keeps nothing locked.
    for _ in 0..1000 {
        assert!(!r.admit(1001), "the oldest is 2");
    }
    assert!(r.admit(1002));
    // A clock that reads earlier than the window admits nothing.
    assert!(!r.admit(0));

    // Any sequence: at most 20 admits in any 1000 ms, and a request is refused only when 20 were admitted in the
    // 1000 ms before it.
    let mut r = RateLimiter::new();
    let mut admitted: Vec<u64> = Vec::new();
    let mut refused = 0;
    let (mut now, mut seed) = (0u64, 0x2545_f491_4f6c_dd1du64);
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        // Mostly bursts a few ms apart, sometimes a pause of up to 400 ms.
        now += if seed % 7 == 0 { seed % 400 } else { seed % 40 };
        let recent = admitted.iter().rev().take_while(|&&a| now - a < 1000).count();
        assert_eq!(r.admit(now), recent < RATE_PER_S, "at {now} with {recent} recent");
        if recent < RATE_PER_S {
            admitted.push(now);
        } else {
            refused += 1;
        }
    }
    assert!(admitted.len() > 1000 && refused > 1000, "both answers: {} admitted, {refused} refused", admitted.len());
}
