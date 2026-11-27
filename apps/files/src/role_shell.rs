//! files.exe --role=shell: the shell host (SH4a). Filled by F-SH; until then the role is not in this build.

/// `args` are the arguments after --role=shell. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let _ = args;
    crate::exit::USAGE
}
