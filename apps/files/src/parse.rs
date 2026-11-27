//! What the user typed in the path field (P3-files-app 2.2; FD-PATH fills it, F4.4). Pure: Env supplies the
//! variables, the profile folder and the known folders; a Stat job decides folder or file afterwards.

use crate::handoff::Target;
use crate::listing::WidePath;
use crate::msg::KnownName;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// An absolute path (a drive, UNC or \\?\ form, or a relative one resolved against the current folder).
    Path(WidePath),
    /// shell:<known name>, resolved through the cached known folders.
    Shell(KnownName),
    /// A location File Explorer opens (the Recycle Bin, Network, This PC, other shell: names, a server alone).
    Virtual(Target),
    /// Text that is not a path: run only through ShellReq::RunTyped, from the user's own Enter.
    Command(String),
    /// The reason it is not a path.
    Invalid(String),
}

/// The environment parse reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    pub vars: Vec<(String, String)>,
    pub profile: Option<WidePath>,
    pub known: Vec<(KnownName, WidePath)>,
}

/// Filled by FD-PATH; until then every input is refused (Invalid with no message).
pub fn parse(input: &str, cwd: &WidePath, env: &Env) -> Parsed {
    let _ = (input, cwd, env);
    Parsed::Invalid(String::new())
}
