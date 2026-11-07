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
Q92 | Override Controls panel needs only the network output switch (engine gates no inputs) | assumed
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
C-QH | After the last workspace closes a home screen shows: links, Open/New/Connect/License buttons, Recent Workspaces/Templates tabs | user
C-QI | Two copies of one file open report different uniqueIDs with the same display name (open of an already-open file not tested) | user
C-QJ | No per-workspace device output delay found; levels are per workspace | user
C-QK | Quit with an unsaved workspace prompts to save, naming it, one prompt per unsaved workspace | user
C-Q10 | `/cue/{n}/stop` stops at once even with a fade-out; `isRunning` false | user
C-Q11 | `/udpReplyPort {port}`: replies and updates move to the new port; after 75 s of silence replies return to the default port; no reply to the command itself | user
C-Q12 | OSC bundles: every message in the bundle runs | user
Q220 | OSC over TCP uses double-ended SLIP framing (RFC 1055, OSC 1.1), no length prefix | manual: osc-dictionary
Q222 | `/playhead` naming a cue that does not exist (or no argument that resolves): `error` reply, playhead unchanged, no state change | assumed (searched: osc-dictionary, open-qlab-questions: manual silent)
Q223 | `/go` with an empty-string argument is looked up as a cue number like any other string: it goes to a cue whose number is exactly empty, else `error` and nothing starts (never a plain GO) | assumed (searched: osc-dictionary: manual silent)
Q224 | Cue `type` names in OSC replies for kinds QLab's docs do not list a name for (everything except Audio, Group, Cue List): our own display-style names (`Fade`, `Start`, `Stop`, `Network`, ...); clients should not depend on them | assumed (searched: osc-dictionary: no names for these kinds)
