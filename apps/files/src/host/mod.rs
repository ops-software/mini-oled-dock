//! What both host roles share: the test hooks (complete), the named pipe (F-OPS, then F-PIPE), process spawning with
//! an explicit inherited-handle list and anonymous pipes (F-SH).

pub mod anon;
pub mod named;
pub mod proc;

/// Ignored entirely unless cfg!(debug_assertions); P3 always passes None.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestHooks {
    pub pipe_tag: Option<Box<str>>, // [a-z0-9]{1,9}: appended to the ops pipe nonce (client and host), so parallel tests never share a host
    pub build_tag: Option<Box<str>>, // 8 hex digits: replaces build_tag() in the pipe name (two_builds_coexist)
    pub env: Vec<(Box<str>, Box<str>)>, // FILES_* variables set on the child process only, never on the test process
}

/// The hooks in force: `h` in a debug build, None in a release build.
pub fn effective(h: &Option<TestHooks>) -> Option<&TestHooks> {
    effective_in(cfg!(debug_assertions), h)
}

fn effective_in(debug: bool, h: &Option<TestHooks>) -> Option<&TestHooks> {
    if debug {
        h.as_ref()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_ignored_in_release() {
        let h = Some(TestHooks {
            pipe_tag: Some("abc123".into()),
            build_tag: None,
            env: vec![("FILES_X".into(), "1".into())],
        });
        assert_eq!(effective_in(false, &h), None, "a release build ignores the hooks");
        assert_eq!(effective_in(true, &h), h.as_ref(), "a debug build uses them");
        assert_eq!(effective_in(true, &None), None);
        assert_eq!(effective(&h).is_some(), cfg!(debug_assertions));
    }
}
