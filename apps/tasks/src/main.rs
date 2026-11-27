//! tasks.exe: everything is in the library (app::main, the start order of F-T2).
#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    tasks::app::main()
}
