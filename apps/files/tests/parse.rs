//! Path parsing (F4.4): one table row per input form of the path field, pure.

use files_engine::handoff::Target;
use files_engine::listing::WidePath;
use files_engine::msg::KnownName;
use files_engine::parse::{parse, Env, Parsed};

fn wp(s: &str) -> WidePath {
    WidePath::parse(s).expect("a made-up path parses")
}

fn env() -> Env {
    Env {
        vars: vec![
            ("MADEUP_HOME".into(), r"D:\made-up\home".into()),
            ("MadeUpTemp".into(), r"D:\made-up\tmp\".into()),
            ("MADEUP_BAD".into(), "bad\u{1}value".into()),
        ],
        profile: Some(wp(r"C:\Users\made-up")),
        known: vec![(KnownName::Downloads, wp(r"C:\Users\made-up\Downloads"))],
    }
}

fn cwd() -> WidePath {
    wp(r"C:\made-up\work\reports")
}

fn path(s: &str) -> Parsed {
    Parsed::Path(wp(s))
}

fn invalid(c: &str) -> Parsed {
    Parsed::Invalid(c.to_string())
}

#[test]
fn every_input_form() {
    let long = format!(r"C:\made-up\{}", "a".repeat(300));
    let too_long = format!(r"C:\made-up\{}", "a".repeat(33_000));
    let rows: Vec<(String, Parsed)> = vec![
        // absolute: drive, UNC, \\?\
        (r"C:\made-up\x".into(), path(r"C:\made-up\x")),
        (r"c:\made-up\x".into(), path(r"c:\made-up\x")),
        (r"D:\".into(), path(r"D:\")),
        ("D:".into(), path(r"D:\")),
        (r"D:\made-up\x\".into(), path(r"D:\made-up\x")),
        (r"\\made-up-srv\share\dir".into(), path(r"\\made-up-srv\share\dir")),
        (r"\\made-up-srv\share".into(), path(r"\\made-up-srv\share")),
        (r"\\?\C:\made-up\x".into(), path(r"C:\made-up\x")),
        (r"\\?\UNC\made-up-srv\share\x".into(), path(r"\\made-up-srv\share\x")),
        (r"\\made-up-srv".into(), Parsed::Virtual(Target::Other(r"\\made-up-srv".into()))),
        (r"\\made-up-srv\".into(), Parsed::Virtual(Target::Other(r"\\made-up-srv\".into()))),
        (r"\\?\UNC\made-up-srv".into(), Parsed::Virtual(Target::Other(r"\\?\UNC\made-up-srv".into()))),
        (r"\\?\GLOBALROOT\x".into(), invalid("device")),
        (r"\\.\pipe\x".into(), invalid("device")),
        (r"\\".into(), invalid("not_absolute")),
        // quotes, spaces, slashes
        (r#""C:\made-up\a b""#.into(), path(r"C:\made-up\a b")),
        (r#"  "C:\made-up\a b"  "#.into(), path(r"C:\made-up\a b")),
        ("  C:\\made-up\\x  ".into(), path(r"C:\made-up\x")),
        ("C:/made-up/x".into(), path(r"C:\made-up\x")),
        (r"C:\made-up\\x".into(), path(r"C:\made-up\x")),
        // dot components
        (r"C:\made-up\a\..\b".into(), path(r"C:\made-up\b")),
        (r"C:\made-up\.\b".into(), path(r"C:\made-up\b")),
        (r"C:\..\..".into(), path(r"C:\")),
        // ~
        ("~".into(), path(r"C:\Users\made-up")),
        (r"~\Docs".into(), path(r"C:\Users\made-up\Docs")),
        ("~/Docs".into(), path(r"C:\Users\made-up\Docs")),
        // %VAR%
        (r"%MADEUP_HOME%\a".into(), path(r"D:\made-up\home\a")),
        (r"%madeup_home%".into(), path(r"D:\made-up\home")),
        (r"%MADEUPTEMP%x".into(), path(r"D:\made-up\tmp\x")),
        (r"%NO_SUCH_VAR%\a".into(), path(r"C:\made-up\work\reports\%NO_SUCH_VAR%\a")),
        ("%MADEUP_BAD%".into(), invalid("control")),
        // shell:
        ("shell:downloads".into(), Parsed::Shell(KnownName::Downloads)),
        ("SHELL:Documents".into(), Parsed::Shell(KnownName::Documents)),
        ("shell:desktop".into(), Parsed::Shell(KnownName::Desktop)),
        ("shell:pictures".into(), Parsed::Shell(KnownName::Pictures)),
        ("shell:music".into(), Parsed::Shell(KnownName::Music)),
        ("shell:videos".into(), Parsed::Shell(KnownName::Videos)),
        ("shell:profile".into(), Parsed::Shell(KnownName::Profile)),
        ("shell:RecycleBinFolder".into(), Parsed::Virtual(Target::RecycleBin)),
        ("shell:NetworkPlacesFolder".into(), Parsed::Virtual(Target::Network)),
        ("shell:MyComputerFolder".into(), Parsed::Virtual(Target::ThisPc)),
        ("shell:Startup".into(), Parsed::Virtual(Target::Other("shell:Startup".into()))),
        ("shell:::{made-up-guid}".into(), Parsed::Virtual(Target::Other("shell:::{made-up-guid}".into()))),
        // rooted and relative
        (r"\made-up\x".into(), path(r"C:\made-up\x")),
        (r"..\x".into(), path(r"C:\made-up\work\x")),
        (r"..\..\x".into(), path(r"C:\made-up\x")),
        (r".\x".into(), path(r"C:\made-up\work\reports\x")),
        (r"x\y".into(), path(r"C:\made-up\work\reports\x\y")),
        ("x/y".into(), path(r"C:\made-up\work\reports\x\y")),
        (".".into(), path(r"C:\made-up\work\reports")),
        ("..".into(), path(r"C:\made-up\work")),
        // long, refused
        (long.clone(), path(&long)),
        (too_long, invalid("too_long")),
        ("C:\\made-up\\a\u{7}b".into(), invalid("control")),
        ("a:b".into(), invalid("not_absolute")),
        ("C:x".into(), invalid("not_absolute")),
        ("".into(), invalid("empty")),
        ("   ".into(), invalid("empty")),
        (r#""""#.into(), invalid("empty")),
        // commands
        ("cmd".into(), Parsed::Command("cmd".into())),
        ("wt -d .".into(), Parsed::Command("wt -d .".into())),
        ("notepad readme".into(), Parsed::Command("notepad readme".into())),
        ("...".into(), Parsed::Command("...".into())),
    ];
    assert!(rows.len() >= 40);
    for (input, want) in rows {
        assert_eq!(parse(&input, &cwd(), &env()), want, "input {input:?}");
    }
}

#[test]
fn unicode_and_unc_cwd() {
    let unc = wp(r"\\made-up-srv\share\dir");
    assert_eq!(parse(r"..\..\..\x", &unc, &env()), path(r"\\made-up-srv\share\x"));
    assert_eq!(parse(r"\x", &unc, &env()), path(r"\\made-up-srv\share\x"));
    assert_eq!(parse("caf\u{e9}\\\u{4e2d}", &cwd(), &env()), path("C:\\made-up\\work\\reports\\caf\u{e9}\\\u{4e2d}"));
}

#[test]
fn no_profile_leaves_tilde_literal() {
    let bare = Env::default();
    assert_eq!(parse("~", &cwd(), &bare), Parsed::Command("~".into()));
}
