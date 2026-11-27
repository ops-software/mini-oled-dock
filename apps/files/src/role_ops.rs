//! files.exe --role=ops: the ops host (OP5c). Filled by F-OPS; until then the role is not in this build.

/// `args` are the arguments after --role=ops. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let _ = args;
    crate::exit::USAGE
}
