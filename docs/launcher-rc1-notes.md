# Launcher 1.5.0-rc1: notes

Draft of the rc1 notes (the text for the pull request that merges `feat/launcher` into `main`). rc1 is the `dock-release` artifact of that pull request's CI run: no tag, no GitHub release, version unchanged (1.4.3), because the updater refuses pre-release versions.

## What works

An elevated launcher inside the dock. It is off by default: set `"launcher": { "chord": "alt+shift+space" }` in the dock config and press the chord. Type to search:

- apps (with a running indicator; Enter on a running app switches to its most recent window)
- open windows (`<`)
- Windows settings
- system commands
- calculator (`=`, or an expression)
- run lines (`>`, with a review of long lines; Run as administrator from the action list)
- web search (`??`, the engine and its template are configurable)
- the registry row (`:`)

Keys typed before the window is up are kept and replayed, Esc and Alt+F4 close it, and a refused launch shows an error row.

## Known limits of rc1

- Screen-reader support is best effort (the window is named "Launcher").
- File search arrives in 1.5.0. The search field's placeholder already mentions files: it is the final text and does not search files yet in rc1.
- Alt+Space is not taken by default. PowerToys Command Palette answers Alt+Space too: turn its shortcut off before moving the launcher to Alt+Space, or keep Alt+Shift+Space. Keep Command Palette if you need file search until 1.5.0.
- No Win-key fail-open is claimed (it ships in 1.5.1).

## Installing rc1

The owner installs it by hand with `dock.exe --install`. While on rc1, leave `update_auto_install` off, or a 1.4.x hotfix would replace it.

## L0 outcomes that change behaviour (fill in after the owner session; none recorded yet)

- Run as administrator for typed lines: off only if S-GESTURE failed.
- Alt+Space release gate: note only if S-ALTSPACE failed or was inconclusive.
- S-OWNER: the launcher currently uses the unowned window (the fallback, off by default in code); the session decides the default.

## Release-string check (run on the rc1 exe, must print nothing)

`Select-String -Path dock.exe -SimpleMatch -Pattern 'DOCK_LAUNCHER_','DOCK_TEST_' -List`

## New Italian strings (for the owner to read)

Reviewed against the dock's terms (Esplora risorse, Esegui come amministratore and Apri percorso file as in Start, Chiudi finestra as in the app popup, Alt+Maiusc+Spazio as Windows names it). One change from the first draft: "Show in folder" / "Mostra nella cartella" became "Open file location" / "Apri percorso file", the wording Start already uses. The last two rows exist only in the preview fixture (the PowerToys notice scene).

| English | Italiano |
|---|---|
| Search apps, files, settings, commands | Cerca app, file, impostazioni, comandi |
| Calc | Calcolo |
| Run | Esegui |
| Windows | Finestre |
| Web | Web |
| Settings | Impostazioni |
| Time | Data e ora |
| Registry | Registro |
| Files | File |
| Open · Enter | Apri · Invio |
| Switch · Enter | Passa · Invio |
| Copy · Enter | Copia · Invio |
| Run · Enter | Esegui · Invio |
| Run in console · Enter | Esegui nella console · Invio |
| Open | Apri |
| Switch to | Passa a |
| Run as administrator | Esegui come amministratore |
| Open file location | Apri percorso file |
| Copy | Copia |
| Close window | Chiudi finestra |
| Forget | Dimentica |
| Run in console | Esegui nella console |
| Run by Windows:  | Eseguito da Windows:  |
| Line too long to review | Riga troppo lunga da verificare |
| No window to act on | Nessuna finestra su cui agire |
| Couldn't start it: Explorer isn't answering | Impossibile avviarlo: Esplora risorse non risponde |
| (thousands) | (migliaia) |
|  · Not responding |  · Non risponde |
| Search {host} for '{q}' | Cerca '{q}' su {host} |
| Type to search {host} | Scrivi per cercare su {host} |
| Open Registry Editor | Apri l'Editor del Registro di sistema |
| New: a launcher for apps, files and commands. Use Alt+Space | Novità: un launcher per app, file e comandi. Usa Alt+Spazio |
| Use Alt+Space | Usa Alt+Spazio |
| Use Alt+Shift+Space | Usa Alt+Maiusc+Spazio |
| Don't show again | Non mostrare più |
| Another app holds Alt+Space. Use Alt+Shift+Space | Un'altra app usa Alt+Spazio. Usa Alt+Maiusc+Spazio |
| Another app holds Alt+Space and Alt+Shift+Space | Un'altra app usa Alt+Spazio e Alt+Maiusc+Spazio |
| Launcher | Launcher |
| PowerToys Command Palette also answers Alt+Space | PowerToys Command Palette risponde anche ad Alt+Spazio |
| Both launchers would open at once. Turn its Alt+Space shortcut off in PowerToys, or keep this launcher on Alt+Shift+Space. | I due launcher si aprirebbero insieme. Disattiva la sua scorciatoia Alt+Spazio in PowerToys, oppure lascia questo launcher su Alt+Maiusc+Spazio. |
