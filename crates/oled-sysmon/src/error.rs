//! Errors of the readers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysmonError {
    /// An NTSTATUS from NtQuerySystemInformation.
    Status(i32),
    /// The snapshot needs more than snapshot::MAX_BUFFER.
    TooLarge { need: u32 },
    /// A bounds or size check of the walk failed.
    Layout { offset: u32, what: &'static str },
    /// The own-process cross-check failed (field drift).
    SelfTest(&'static str),
    /// GetLastError of a Win32 call.
    Win32(u32),
}

impl SysmonError {
    pub fn code(&self) -> i32 {
        match *self {
            SysmonError::Status(s) => s,
            SysmonError::TooLarge { .. } => 0xC000_0004u32 as i32,
            SysmonError::Layout { .. } => 0xE000_0001u32 as i32,
            SysmonError::SelfTest(_) => 0xE000_0002u32 as i32,
            SysmonError::Win32(e) => (0x8007_0000u32 | (e & 0xFFFF)) as i32,
        }
    }
}

impl std::fmt::Display for SysmonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SysmonError::Status(s) => write!(f, "system query failed with status {:#010x}", *s as u32),
            SysmonError::TooLarge { need } => write!(f, "process snapshot too large ({need} bytes)"),
            SysmonError::Layout { offset, what } => {
                write!(f, "process snapshot layout check failed at {offset}: {what}")
            }
            SysmonError::SelfTest(what) => write!(f, "own-process cross-check failed: {what}"),
            SysmonError::Win32(e) => write!(f, "Win32 error {e}"),
        }
    }
}

impl std::error::Error for SysmonError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(SysmonError::Status(-5).code(), -5);
        assert_eq!(SysmonError::Status(0x1234).code(), 0x1234);
        assert_eq!(SysmonError::TooLarge { need: 1 }.code() as u32, 0xC000_0004);
        assert_eq!(SysmonError::Layout { offset: 0, what: "x" }.code() as u32, 0xE000_0001);
        assert_eq!(SysmonError::SelfTest("x").code() as u32, 0xE000_0002);
        assert_eq!(SysmonError::Win32(5).code() as u32, 0x8007_0005);
        assert_eq!(SysmonError::Win32(0x1_0005).code() as u32, 0x8007_0005);
    }

    #[test]
    fn display_is_english() {
        assert!(SysmonError::Win32(5).to_string().contains("Win32"));
    }
}
