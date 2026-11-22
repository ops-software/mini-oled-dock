# launcher-probe

Read-only probe for the launcher's L0 owner session (plan L1, task L0-1). It prints JSON lines to stdout and writes
nothing anywhere else: no file, no registry value, no window message, no keyboard hook, no handle opened for writing
(processes are opened with PROCESS_QUERY_LIMITED_INFORMATION, tokens with TOKEN_QUERY; the shell user's known-folder
lookup in `wsi` also asks for the duplicate and impersonate token rights, as the dock's start_import.rs does).

Build (own Cargo project, not part of the dock's build):

    CARGO_TARGET_DIR=target-spike cargo build --release --locked      (run in tools/owner/launcher-probe)
    CARGO_TARGET_DIR=target-spike cargo test --locked

Run elevated, redirect stdout to a private file; the output holds paths of the owner's profile and must never be
committed (technical facts only go to docs/spikes/launcher-l0/).

| Subcommand | What it answers |
|---|---|
| `sid` | user SID of the tool's token and of the shell window's process; `differ` (Administrator protection, SPEC 5.4.6) |
| `wsi [--route a\|b] <words>` | S-WSI. Route a: ISearchManager, GetCatalog("SystemIndex"), query helper bound to the three SCOPE clauses (shell user's profile, Start Menu Programs, common Start Menu Programs), GenerateSQLFromUserQuery, executed through OLE DB (IDataInitialize on the helper's connection string) on an MTA thread. Route b: hand-written SQL on Search.CollatorDSO. Without `--route` route a is tried and route b follows if a fails. Prints each row (max 50) with elapsed ms, `first20_ms`, and private bytes before, during, after the rows, after the thread ended and 60 s later. The windows-crate feature used is `Win32_System_Search` (it compiled with `Win32_System_Com`; nothing else was needed for the interfaces). |
| `pt` | running PowerToys*.exe and Microsoft.CmdPal*.exe with full image paths (S-PT names) |
| `links` | shell:AppsFolder entries that are .url links or folders (named links, SPEC 5.1) |
| `fgwatch <seconds>` | S-PT fallback: foreground changes as {"t": tick, "image": exe file name}; no path, no title, no key hook |
| `cmdpal-items <file>...` | only with the owner's yes to Q3: reads the given Command Palette files read-only (no path is guessed) and prints leaf values under bookmark, extension, usage, count and enabled keys. Output stays in the private results file. |

Open point for the owner session: the route a where-restriction is passed as `AND (<scope clauses>)`; if the helper
rejects that form the session records the error line and route b runs.

## Synthetic keys in the dock's owner-probe build

The dock built with `--features owner-probe` (never a release the owner installs) has one switch for a session
driven by a test agent instead of physical presses: `DOCK_LAUNCHER_ACCEPT_INJECTED=1`, read once when the keyboard
hook starts. With it, the hook treats keys injected by other apps (LLKHF_INJECTED without the dock's own INJECT_TAG)
as physical, for everything it does with them: the Win key, the launcher chord, the type-ahead and the gesture
evidence of Ctrl+Shift+Enter. Its purpose is the L0 session (L0-2) run with SendInput while the owner is away. Every
key the dock injects itself carries that tag (the F15 of its foreground fallback too: launcher lint 13) and still
passes through untouched. The dock log says so at start (`launcher probe: injected keys count as physical`). A build
without the feature has no such code path and the variable name is not in its binary: every release ignores injected
keys.

Results obtained with synthetic keys must be labelled as such: they show what the dock does with SendInput, not with
a person's key presses (for example the hook's kb.time of a synthetic key is stamped by SendInput).
