---
name: qlab-behaviour
description: "Load before deciding, building or reviewing any behaviour CLAUDE.md says to match from QLab 5, and before asking the user a QLab question. Holds every user answer and every assumed decision (cues, panic, fades, OSC, levels, files, Show Mode, cue-list UI). Search with grep first."
---

# QLab 5 behaviour decisions

This skill is the only store of QLab decisions. Every user answer is here: **never ask the user a QLab question already answered, and never re-open an `assumed` row without new evidence.** A reviewer must not report a behaviour as a bug when a row here decides it.

## Lookup order (CLAUDE.md "Behaviour")
1. Search the rows: `grep -rniE "keyword|keyword" .claude/skills/qlab-behaviour/references/` (try 2-3 synonyms: "pause|paused", "broken", "playhead|standby").
2. Then QLab's manual: `~/qlab-ref/qlab-ref search "<words>"`, `show <page>`; then https://qlab.app/docs/v5/. UI layout: `~/qlab-ref/ui`.
3. Still unknown: implement the likeliest behaviour and add an `assumed` row. On a show-safety path, stop and report instead.

## Row format
`Q<id> | decision | source` one line, under 500 characters. Ids are stable: never renumber; cite a row by its id.
- `Q<n>`: numbered decision. `Q<n>b`: a second row that once shared an id. `C-<x>`: a check the user ran in QLab 5.5.3 on 2026-10-27 (a verdict "different" meant openLab changed to follow QLab).
- Source: `user <date>` (the user answered or confirmed), `manual p.N` or `manual: <web page>` (documentation settles it), `assumed` (no user answer; optional date).
- `assumed (asked <date>, unanswered)`: the user was asked and did not answer. Proceed on the row; do not ask again unless the code contradicts it.
- A row holding a deviation says so: "deliberate deviation" with the user decision (Q16).

## Adding a row
- Next free id: highest `Q` number plus one, in a new line in the reference file for the subsystem (below). Check ids with `grep -rhoE "^Q[0-9]+b?" references | sort -V | tail`.
- Content: the situation, the behaviour implemented, and the source. Add "(searched: ...)" in under ten words when the manual was silent.
- Paraphrase QLab; never copy its text, labels, tooltips or screenshots (public repo). Cite pages, not sentences. No personal account or machine details.
- When a user answers, change the row's source to `user <date>` and record how the answer changed behaviour; if it differs from the code, report the code change to the orchestrator.
- Merged duplicates note the old ids as `m: Q<n>`.

## Standing rules these rows rely on
- QLab wins over openLab extras; where QLab differs from a check-page "openLab did" column, the QLab answer was adopted.
- Where `assumed` rows disagree with built code, the code and tests win: fix the row.
- Safety exceptions are deliberate and marked: a Fade does not stop its target when an edit takes it over (Q23); white-space-only level boxes are no change (Q103); hung disks give up after 5 s (Q44).

## Reference files (read the one for your subsystem)
- `references/engine-cues.md`: cue, group, playlist, pause, panic, fade, devamp, preview, trigger, undo-while-playing semantics and their check-page answers (C-G, C-D, C-F, C-P, C-S, C-V). Read for any `crates/core` cue-engine or control-thread change.
- `references/osc-workspaces.md`: OSC addressing and replies, several open workspaces, `/updates`, Network cues and patches, output override. Read for `osc` or Network-cue work.
- `references/levels-patches.md`: level limits, global master, level boxes, patch changes and clearing, routing matrix, loop/trim floors. Read for levels, patches and Audio-cue routing.
- `references/files-modes.md`: name limits, Show Mode saving, autosave and recovery, restore at launch, file references, missing/hung media, device unplug. Read for persistence, media and launch work.
- `references/mac-ui.md`: keys and hotkeys, playhead/selection/typing focus policy, cue list columns, inspector and notes, Show Mode layout, window frames. Read for any `mac/` change.
