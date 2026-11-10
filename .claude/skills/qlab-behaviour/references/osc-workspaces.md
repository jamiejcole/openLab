# OSC, multiple workspaces, Network cues

Q12 | Network cue with a malformed OSC message or a destination naming no patch: edit accepted, cue shows broken (`network_message_invalid` / `network_patch_missing`), never sends. Only memory bounds are refused (64 KiB address, 1024 args, 64 KiB string, non-finite float) | manual p.371-372
Q15 | `/workspace/{name}/...` where two open workspaces share a display name: refused with the same bare `error` as an unknown workspace (never guess: a Finder copy keeps its name and GO must not fire the backup); id matches before name; one warning per peer and name | assumed (asked 2026-10-29, test inconclusive)
Q16 | No workspace open: un-prefixed messages and `/connect` get `error` with only `address` and `status`. QLab gives NO reply to `/version`, `/workspaces`, `/updates 1` then; openLab deliberately still answers `/workspaces` with `[]` and `/version` with its version. `/connect` in QLab: status ok with error in data | user 2026-10-29: leave as is (deviation by decision)
Q17 | Un-prefixed `/updates 1` or `/udpReplyPort` with several workspaces: applies to every open workspace (reply port only to those the peer may view); no reply on success, one `error` (`denied` for `/udpReplyPort` without View anywhere) on failure | assumed (asked 2026-10-29, unanswered)
Q24 | `/updates` and `/udpReplyPort` are per client (application level): a workspace opened later gets them for peers that asked while the peer's entry lives (60 s UDP silence); closing a doc does not lose them | assumed
Q43 | Network patch is incomplete (cues broken `network_host_invalid`) when host is empty/unusable (not IPv4/IPv6 or valid host name) or port 0. A host name that does not resolve or an unreachable destination never breaks a cue | assumed (manual p.371-372, p.112)
Q54 | New workspace starts with one network patch "Network 1" to this Mac (127.0.0.1, UDP 53000). A workspace with none gets that patch added in the same undo step when a Network cue is created. New cue starts with an empty address, so it shows broken and sends nothing | manual p.111 for new workspaces; fallback assumed
Q90 | Output override is engine-memory only: a relaunch or engine restart starts with network output on | assumed
Q91 | Network cue under override shows a red circle in the status column | manual p.162
Q92 | Our "Output Overrides" control needs only the network output switch, since the engine gates no inputs (QLab's counterpart: surfaces.json `window.override-controls`) | assumed
Q109 | App starts while the engine already runs workspaces (app crash): the launch choice is not applied, the engine's workspaces are the show; an engine with none gets the choice | assumed 2026-11-05
Q115 | Network message over the size bounds typed in the inspector: edit refused, field shows the cue's text again, a line says why | assumed
Q116 | Network inspector Send on a disarmed or broken cue sends nothing. `\I` and `\N` (impulse, nil) arguments are refused with a message (not supported yet). Target column shows the patch name | assumed

## Check pages (multi-workspace and OSC), 2026-10-27
C-QA | Un-prefixed OSC goes to every open workspace whichever window is in front; queries get one reply per workspace | user
C-QB | `/workspaces` with two open: one reply listing both in open order; entries carry name, uniqueID, version, port, reply port | user
C-QC | Un-prefixed `/connect` goes to every workspace; each checks its own passcode; access is per workspace | user
C-QD | Un-prefixed `/updates 1` subscribes in every workspace, but a workspace sends updates only once the session is connected to it; `/workspace/{id}/updates 1` errored | user
C-QE | Esc (Panic) stops both workspaces whichever is in front; Pause All pauses only the front one; no all-workspaces command | user
C-QF | A hotkey of a workspace not in front still starts its cue | user
C-QG | Closing a workspace while cues play asks for confirmation | user
C-QH | After the last workspace closes a start screen appears with ways to open, create or connect to a workspace and lists of recent workspaces and templates (surfaces.json `launcher`) | user
C-QI | Two copies of one file open report different uniqueIDs with the same display name (open of an already-open file not tested) | user
C-QJ | No per-workspace device output delay found; levels are per workspace | user
C-QK | Quit with an unsaved workspace prompts to save, naming it, one prompt per unsaved workspace | user
C-Q10 | `/cue/{n}/stop` stops at once even with a fade-out; `isRunning` false | user
C-Q11 | `/udpReplyPort {port}`: replies and updates move to the new port; after 75 s of silence replies return to the default port; no reply to the command itself | user
C-Q12 | OSC bundles: every message in the bundle runs | user
Q220 | OSC over TCP uses double-ended SLIP framing (RFC 1055, OSC 1.1), no length prefix | manual: osc-dictionary
Q222 | `/playhead` naming a cue that does not exist (or no argument that resolves): `error` reply, playhead unchanged, no state change | assumed (searched: osc-dictionary, open-qlab-questions: manual silent)
Q223 | `/go` with an empty-string argument answers `error`, nothing starts, never a plain GO: no cue can have an empty number (validate.rs, command.rs, command/structure.rs refuse it) | assumed (searched: osc-dictionary: manual silent)
Q224 | Cue `type` names in OSC replies for kinds QLab's docs do not list a name for (everything except Audio, Group, Cue List): our own display-style names (`Fade`, `Start`, `Stop`, `Network`, ...); clients should not depend on them | assumed (searched: osc-dictionary: no names for these kinds)

## OSC passcode table in the workspace settings (NET-02)
Q227 | A passcode added with the new-passcode button starts with View, Edit and Control all on, like the new workspace's generated row; its code is fresh (1000-9999, not in the table). The app takes passcodes of exactly four digits, as QLab's manual says; the engine keeps accepting 1-32 digits from commands | manual p.112-113 (starting rights assumed; searched: manual p.112-113 silent on a new row's rights)
Q228 | Unchecking View clears Edit and Control in the same command (the engine refuses Edit or Control without View); checking View again leaves them off. Edit and Control boxes are greyed while View is off | assumed (searched: manual p.112 rights table: silent on the order of clearing)
Q229 | Undo and Redo in the settings window: Edit > Undo and Redo act on the workspace's edits while its settings window is in front; each table edit (add, delete, rights, a code change) is one undo step. A code change is a remove and an add of the row at its place with its rights, in one transaction | assumed (searched: manual pp.96, 111-113: silent on undo in settings)
Q230 | A full table (16 passcode rows, the engine's limit) answers the new-passcode button with a line in the settings window and adds nothing; a refused code edit (not four digits, or already in the table) shows the old code again with a line saying why | assumed (searched: manual p.112-113: names no limit and no refusal text)
