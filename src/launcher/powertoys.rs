//! Leaving PowerToys (SPEC-launcher 5.7): which running programs hold the chord by their own keyboard hook, and the
//! policy the launcher follows while one runs; and the remote-desktop and VM clients the chord goes to while one of them
//! is in front (5.2.13). Pure: the process list, the chord's outcome and the foreground events come from the caller
//! (launcher_sys.rs finds the processes and waits on their exit, popup_launcher.rs feeds the events); nothing of
//! PowerToys is read, changed or ended.

/// The image file names of the programs that answer Alt+Space with a low-level keyboard hook: PowerToys Command Palette
/// and PowerToys Run. S-PT did not record them on the owner's machine (PowerToys was closed for the L0 session), so these
/// are the spec's placeholders. PowerToys.exe, the runner of every module, never counts alone: the owner may keep other
/// modules with Command Palette off.
pub const CHORD_OWNERS: [&str; 2] = ["Microsoft.CmdPal.UI.exe", "PowerToys.PowerLauncher.exe"];

/// How long after a chord passed under YIELD a window of a chord owner must come to the foreground for the chord to count
/// as answered. S-PT did not measure Command Palette's delay, so this is the spec's default; a measured delay of 400 ms or
/// more would make it 1.5 x that delay, at most 1,000 ms.
pub const ANSWER_MS: u32 = 400;

/// Unanswered passes in a row that make the launcher take the chord.
pub const MAX_UNANSWERED: u8 = 2;

/// The image file names of the remote-desktop and VM clients that send key combinations to their session (5.2.13, (u)):
/// Remote Desktop Connection, the Remote Desktop client of Windows App and Azure Virtual Desktop, and Hyper-V's Virtual
/// Machine Connection. While a window of one of them is in front, the hook's REMOTE flag passes the chord to the session.
pub const REMOTE_CLIENTS: [&str; 3] = ["mstsc.exe", "msrdc.exe", "vmconnect.exe"];

/// The file name of a full image path (after the last `\` or `/`) is one of `names`, ASCII case ignored. UTF-16 in, as
/// Windows hands the path over: nothing is allocated per process.
fn file_name_in(path: &[u16], names: &[&str]) -> bool {
    let sep = |c: &u16| *c == u16::from(b'\\') || *c == u16::from(b'/');
    let name = path.rsplit(sep).next().unwrap_or(path);
    names.iter().any(|known| {
        known.len() == name.len()
            && known.bytes().zip(name).all(|(k, &n)| u8::try_from(n).is_ok_and(|n| n.eq_ignore_ascii_case(&k)))
    })
}

/// The image is one of `CHORD_OWNERS`.
pub fn is_chord_owner(path: &[u16]) -> bool {
    file_name_in(path, &CHORD_OWNERS)
}

/// The image is one of `REMOTE_CLIENTS` (the foreground window's process, at each foreground change).
pub fn is_remote_client(path: &[u16]) -> bool {
    file_name_in(path, &REMOTE_CLIENTS)
}

/// Where the launcher stands with a chord owner (5.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PtState {
    /// No chord owner runs: the chord is the launcher's.
    Absent,
    /// One runs and its notice was never answered: the next chord the dock sees opens the launcher with the notice.
    Detected,
    /// The chord passes to it (the hook's YIELD flag) as long as it answers; `unanswered` passes in a row so far.
    Yield { unanswered: u8 },
    /// The launcher keeps the chord: `with_powertoys` is "take", or the owner stopped answering.
    Take,
}

/// What happens to the state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PtEvent {
    /// A check found a chord owner running.
    Found,
    /// A check found none (the last one exited).
    Gone,
    /// The dock saw the chord and opens the launcher.
    FirstChord,
    /// `with_powertoys` became "take".
    ChoseTake,
    /// `with_powertoys` became "yield".
    ChoseYield,
    /// The hook passed the chord under YIELD: its answer window opens.
    PassedChord,
    /// A window of a chord owner came to the foreground inside the answer window.
    OwnerAnswered,
    /// The answer window (`ANSWER_MS`) ended with no such window.
    NoAnswerIn400ms,
}

/// A notice row the next open shows (one each, once).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PtNotice {
    /// "PowerToys Command Palette is running and also uses Alt+Space.", with its choices.
    Shared,
    /// "Command Palette did not answer Alt+Space", with the detail "It is running, but the launcher has Alt+Space now".
    TookOver,
}

/// The policy state machine of 5.7, with the config's two keys (`with_powertoys` and `powertoys_done`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pt {
    state: PtState,
    /// `with_powertoys` is "take".
    take: bool,
    /// `powertoys_done`: the notice was answered once, so a chord owner found again goes straight to Yield.
    done: bool,
    /// A passed chord's answer window is open.
    watching: bool,
    pending: Option<PtNotice>,
}

impl Default for Pt {
    fn default() -> Pt {
        Pt::new()
    }
}

impl Pt {
    pub const fn new() -> Pt {
        Pt { state: PtState::Absent, take: false, done: false, watching: false, pending: None }
    }

    pub fn state(&self) -> PtState {
        self.state
    }

    /// The hook passes the chord (its YIELD flag).
    pub fn yields(&self) -> bool {
        matches!(self.state, PtState::Yield { .. })
    }

    /// A passed chord waits for its answer.
    pub fn watching(&self) -> bool {
        self.watching
    }

    /// The notice the next open shows, once.
    pub fn take_notice(&mut self) -> Option<PtNotice> {
        self.pending.take()
    }

    /// The config's keys, at start and at each change: a changed `with_powertoys` is the choice of the notice (or of the
    /// config file); `powertoys_done` set before the first chord means no notice.
    pub fn configure(&mut self, take: bool, done: bool) {
        let was_take = std::mem::replace(&mut self.take, take);
        self.done = done;
        if take && !was_take {
            self.on(PtEvent::ChoseTake);
        } else if !take && was_take {
            self.on(PtEvent::ChoseYield);
        }
        if done && self.state == PtState::Detected {
            self.state = PtState::Yield { unanswered: 0 };
        }
    }

    /// The state a newly found chord owner leaves the launcher in.
    fn found(&self) -> PtState {
        if self.take {
            PtState::Take
        } else if self.done {
            PtState::Yield { unanswered: 0 }
        } else {
            PtState::Detected
        }
    }

    pub fn on(&mut self, ev: PtEvent) {
        use PtEvent::*;
        use PtState::*;
        self.state = match (self.state, ev) {
            (_, Gone) => {
                self.watching = false;
                self.pending = None;
                Absent
            }
            (Absent, Found) => self.found(),
            // Taken once, to show the notice; from then on the chord passes (the notice's choice may change that).
            (Detected, FirstChord) => {
                self.pending = Some(PtNotice::Shared);
                Yield { unanswered: 0 }
            }
            (Absent, ChoseTake | ChoseYield) => Absent,
            (_, ChoseTake) => {
                self.watching = false;
                Take
            }
            (Take | Detected, ChoseYield) => Yield { unanswered: 0 },
            (Yield { unanswered }, PassedChord) => {
                self.watching = true;
                Yield { unanswered }
            }
            (Yield { .. }, OwnerAnswered) if self.watching => {
                self.watching = false;
                Yield { unanswered: 0 }
            }
            (Yield { unanswered }, NoAnswerIn400ms) if self.watching => {
                self.watching = false;
                let n = unanswered.saturating_add(1);
                if n >= MAX_UNANSWERED {
                    self.pending = Some(PtNotice::TookOver);
                    Take
                } else {
                    Yield { unanswered: n }
                }
            }
            (s, _) => s,
        };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use PtEvent::*;
    use PtState::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// A check over a fixture process list: Found when one image is a chord owner, Gone otherwise.
    fn check(pt: &mut Pt, images: &[&str]) {
        let found = images.iter().any(|i| is_chord_owner(&w(i)));
        pt.on(if found { Found } else { Gone });
    }

    const CMDPAL: &str = "C:\\Program Files\\PowerToys\\WinUI3Apps\\Microsoft.CmdPal.UI.exe";
    const RUNNER: &str = "C:\\Program Files\\PowerToys\\PowerToys.exe";
    const NOTEPAD: &str = "C:\\Windows\\System32\\notepad.exe";

    #[test]
    fn remote_flag_from_image_names() {
        // The foreground process's image as QueryFullProcessImageNameW gives it: the input of the REMOTE pass flag.
        let remote = [
            "C:\\Windows\\System32\\mstsc.exe",
            "C:\\Program Files\\Remote Desktop\\msrdc.exe",
            "C:\\Windows\\System32\\vmconnect.exe",
            "c:/windows/system32/MSTSC.EXE",
            "msrdc.exe",
        ];
        for image in remote {
            assert!(is_remote_client(&w(image)), "{image}");
            assert!(!is_chord_owner(&w(image)), "{image}: a remote client is not a chord owner");
        }
        let local = [
            NOTEPAD,
            CMDPAL,
            RUNNER,
            "C:\\x\\mstsc.exe.lnk",
            "C:\\x\\xmstsc.exe",
            "C:\\mstsc.exe\\notepad.exe",
            "C:\\x\\vmconnect",
            "",
        ];
        for image in local {
            assert!(!is_remote_client(&w(image)), "{image}");
        }
    }

    #[test]
    fn chord_owners_by_file_name() {
        assert!(is_chord_owner(&w(CMDPAL)));
        assert!(is_chord_owner(&w("c:/x/powertoys.powerlauncher.EXE")), "case and either separator");
        assert!(is_chord_owner(&w("Microsoft.CmdPal.UI.exe")), "a bare name");
        assert!(!is_chord_owner(&w(RUNNER)), "the runner alone never counts");
        assert!(!is_chord_owner(&w("C:\\x\\Microsoft.CmdPal.UI.exe.bak")));
        assert!(!is_chord_owner(&w("C:\\Microsoft.CmdPal.UI.exe\\notepad.exe")), "only the file name");
        assert!(!is_chord_owner(&w("C:\\x\\Microsoft.CmdPal.UI.ex\u{e9}")), "no ASCII folding of other letters");
        assert!(!is_chord_owner(&[]));
    }

    #[test]
    fn state_machine() {
        // PowerToys.exe alone: nothing holds the chord.
        let mut pt = Pt::new();
        check(&mut pt, &[NOTEPAD, RUNNER]);
        assert_eq!(pt.state(), Absent);
        assert!(!pt.yields());
        // Detected: the chord is still taken (no YIELD), the first one opens the notice once.
        check(&mut pt, &[NOTEPAD, RUNNER, CMDPAL]);
        assert_eq!(pt.state(), Detected);
        assert!(!pt.yields(), "the dock must see the first chord");
        check(&mut pt, &[CMDPAL]);
        assert_eq!(pt.state(), Detected, "found again: no change");
        pt.on(FirstChord);
        assert_eq!(pt.take_notice(), Some(PtNotice::Shared));
        assert_eq!(pt.take_notice(), None, "once");
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        assert!(pt.yields());
        pt.on(FirstChord);
        assert_eq!(pt.take_notice(), None, "never a second notice");
        // Yield: a pass that is answered keeps the count at 0.
        pt.on(PassedChord);
        assert!(pt.watching());
        pt.on(OwnerAnswered);
        assert!(!pt.watching());
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        // An answer outside a window counts for nothing.
        pt.on(OwnerAnswered);
        pt.on(NoAnswerIn400ms);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        // One unanswered pass, then an answered one: the count goes back to 0.
        pt.on(PassedChord);
        pt.on(NoAnswerIn400ms);
        assert_eq!(pt.state(), Yield { unanswered: 1 });
        pt.on(PassedChord);
        pt.on(OwnerAnswered);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        // Two unanswered passes in a row: the launcher takes the chord and says so at the next open.
        pt.on(PassedChord);
        pt.on(NoAnswerIn400ms);
        pt.on(PassedChord);
        pt.on(NoAnswerIn400ms);
        assert_eq!(pt.state(), Take);
        assert!(!pt.yields() && !pt.watching());
        assert_eq!(pt.take_notice(), Some(PtNotice::TookOver));
        pt.on(PassedChord);
        assert!(!pt.watching(), "nothing passes under Take");
        // Exit: the chord is ours, with no notice left.
        check(&mut pt, &[NOTEPAD, RUNNER]);
        assert_eq!(pt.state(), Absent);
        // Found again: still not answered, so Detected again.
        check(&mut pt, &[CMDPAL]);
        assert_eq!(pt.state(), Detected);
    }

    #[test]
    fn choices_and_config() {
        // Take (Enter on the notice): with_powertoys "take".
        let mut pt = Pt::new();
        check(&mut pt, &[CMDPAL]);
        pt.on(FirstChord);
        pt.configure(true, true);
        assert_eq!(pt.state(), Take);
        assert!(!pt.yields());
        // Exit and found again: "take" holds.
        check(&mut pt, &[]);
        check(&mut pt, &[CMDPAL]);
        assert_eq!(pt.state(), Take);
        // Back to "yield" in the config: the chord passes again, with the answer check.
        pt.configure(false, true);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        // Leave it / Don't show again: answered, so a later find yields at once (no second notice).
        let mut pt = Pt::new();
        check(&mut pt, &[CMDPAL]);
        pt.on(FirstChord);
        let _ = pt.take_notice();
        pt.configure(false, true);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        check(&mut pt, &[]);
        check(&mut pt, &[CMDPAL]);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        assert_eq!(pt.take_notice(), None);
        // Answered before any chord (the config file says so): no notice at all.
        let mut pt = Pt::new();
        check(&mut pt, &[CMDPAL]);
        pt.configure(false, true);
        assert_eq!(pt.state(), Yield { unanswered: 0 });
        // "take" set while nothing runs: found later, taken at once.
        let mut pt = Pt::new();
        pt.configure(true, false);
        assert_eq!(pt.state(), Absent);
        check(&mut pt, &[CMDPAL]);
        assert_eq!(pt.state(), Take);
        // The same config again changes nothing (a config reload while the owner stopped answering).
        let mut pt = Pt::new();
        pt.configure(false, true);
        check(&mut pt, &[CMDPAL]);
        for _ in 0..MAX_UNANSWERED {
            pt.on(PassedChord);
            pt.on(NoAnswerIn400ms);
        }
        assert_eq!(pt.state(), Take);
        pt.configure(false, true);
        assert_eq!(pt.state(), Take, "the launcher keeps the chord it took");
        // A notice of an owner that exited is dropped.
        let mut pt = Pt::new();
        check(&mut pt, &[CMDPAL]);
        pt.on(FirstChord);
        check(&mut pt, &[]);
        assert_eq!(pt.take_notice(), None);
    }

    #[test]
    fn the_answer_window_and_its_bounds() {
        assert_eq!(ANSWER_MS, 400, "S-PT unmeasured: the spec's default");
        assert_eq!(MAX_UNANSWERED, 2);
        assert_eq!(CHORD_OWNERS.len(), 2);
    }
}
