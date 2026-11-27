//! LiveRoot (examples/common/live.rs) refuses every folder that is not a live tester's root. Nothing is created
//! outside a TempRoot: the refusals are checked on a TempRoot, on %TEMP% itself and on absent paths.

#[path = "../examples/common/live.rs"]
mod live;

use live::temp::TempRoot;
use live::LiveRoot;

#[test]
fn live_root_refuses_foreign_folders() {
    let a = TempRoot::new("live_refuses");
    assert!(LiveRoot::at(a.path()).is_err(), "a test root is not a live root");
    assert!(LiveRoot::at(&a.dir("mod-files-live-inner")).is_err(), "a live-looking folder inside a test root");
    assert!(LiveRoot::at(&std::env::temp_dir()).is_err());
    let absent = std::env::temp_dir().join(format!("mod-files-live-absent-{}", std::process::id()));
    assert!(LiveRoot::at(&absent).is_err(), "an absent folder is refused");
    assert!(LiveRoot::at(std::path::Path::new("mod-files-live-x")).is_err(), "a relative path is refused");
}
