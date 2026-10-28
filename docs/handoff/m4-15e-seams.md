<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# M4.15e seams left by M4.15d

M4.15d split `Control` into a shared `Engine` and one `Doc` (docs/control.md "Engine and docs"). With exactly one doc nothing below misbehaves. Each item is a place that assumes one doc; each was marked `// M4.15e:` (with the letter below) in `engine/crates/engine/src/control/`. M4.15e (and M4.15d1, which points here) must fix them before a second doc is installed.

**All resolved in M4.15e** (the `// M4.15e:` markers are gone; docs/control.md "Several docs" describes the result). Per item, how:

- (a) `mapping_warned` is keyed `(DocId, WorkspaceId, PatchId, device)`; a doc's retain drops only its own stale entries (the one-doc rule unchanged), a closed doc's go at close.
- (b) `Control::device_restored` sets the new lookahead in every doc's cue engine before `eng.block` changes; if one cannot, all are set back to the old block and one warning is logged.
- (c) the unsignalled-change high-water mark is per doc (`Doc::seen_unsignalled`).
- (d) `Control::begin_meter_round` reads the output and voice meters once per report period into a `MeterRound`; each doc's `reports` takes its cues' peaks, its patches' outputs and reconciles its started voices against the voices listed; `last_progress` and `meters_live` stay per doc.
- (e) `Patches` keeps one free list and per-doc slot maps keyed `(DocId, PatchId)`; a slot belongs to one doc's patch, so that doc's `slots_in_use` is all that can hold it back (no cross-doc union is needed); `release_doc` frees a closed doc's slots, and docs waiting for a slot sync again on the next wake (`take_freed`).
- (f) `Control::tick` runs the device poll, the mixer event drain (routed by `VoiceIds::owner`), the watch, the meter read, the probe watchdog, slot hand-outs, `check_stalled_read`, `poll_clears`, settings, health and status once per wake, and works with zero docs; device loss/return, health's resends and shutdown loop over the docs for their parts.
- (g) `Inbox::Probe(DocId, _)` and `Inbox::Net(DocId, _)` are tagged and dropped for a closed doc. `Inbox::Doc` needs no tag: one read runs at a time engine-wide, its ticket names the pending request, and the request names its doc (`Target`), so an answer for a closed doc answers `no_workspace`. `Inbox::Watch` needs none either: the one watch's events name files, and `WatchShare` hands each to every doc that watches it.
- (h) `Observe`'s cue states are keyed `(DocId, CueId)` and reset per doc; the crash hook gets every open doc's show path (in open order, `; `-separated; `None` when none is saved, as before).

Placement moves, done: the probe pool is the engine's (`ProbePool`, 2 workers, jobs and answers tagged); the media watch is one `MediaWatch` behind `WatchShare` with per-file counts; `NetOut` stays per doc, with the look-up bound (`LookupThreads`) and the PB-10 flag shared by the engine.

## Single-doc assumptions

- **(a) `mapping.rs`, `map_unmapped_patches`**: `mapping_warned.retain` keeps only this doc's entries, wiping the other docs'. Fix: drop only this doc's stale entries (those with this workspace id whose patch is gone).
- **(b) `mod.rs`, `device_restored`**: on a reopen with a new block size, `set_lookahead` runs on one doc's cue engine and then `eng.block` changes, so the other docs keep a stale lookahead. Fix: set it on every doc first and change `eng.block` only if all succeed (or warn per doc).
- **(c) `mod.rs`, `feed`**: `eng.seen.unsignalled` is an engine-wide high-water mark compared with one doc's `unsignalled_model_changes`. Fix: keep the counter per doc.
- **(d) `mod.rs`, `reports` / `reconcile`**: `last_meters` and `last_meter_read` are engine fields consumed by a per-doc report. The first doc's read sets them, so doc B gets no meters and no lost-voice reconciliation. Fix: read the mixer meters once per period in the engine and hand each doc its voices' peaks and the listed ids. `meters_live` and `last_progress` stay per doc.
- **(e) `mod.rs`, `sync_patches`**: `Patches::sync` is keyed by `PatchId` only. ARCHITECTURE §3.5 keys slots by `(DocId, PatchId)`, and the slots in use (`Voices::slots_in_use`, also in `reclaim_slots`) must span every doc.
- **(f) Engine-wide steps run through a doc**: `check_stalled_read` and `poll_clears` (in `docs_tick`), the mixer event drain (`DocCx::tick`), `poll_device`, `settings_tick`, `health` and `observe_tick` (`Control::tick`) and `shutdown` all run through the one doc's `DocCx`. With N docs they would run N times; with 0 docs (M4.15g) never. Fix: run them once on the engine and loop over the docs only for the doc parts (device loss stops every doc; health's resends are per doc; drain mixer events once and route by voice owner; shutdown panics every doc then pumps).
- **(g) `inbox.rs`**: `Probe`, `Net`, `Watch` and `Doc` answers carry no `DocId`. Fix: tag the jobs and route the answers; an answer for a closed doc is ignored.
- **(h) `docs.rs`, `install`**: `eng.observe.reset()` on any doc's install forgets every doc's logged cue states. Fix: key `Observe`'s per-cue states (and the crash-hook workspace path) by doc.

## Placement directions (from the M4.15d review)

- **Probe pool**: the workers, their watchdog and counters move from `MediaCatalog` into `Engine` (ARCHITECTURE §3.1: one pool of 2). Jobs and answers are tagged with their `DocId`. The catalog (entries, budget, cue index) stays per doc.
- **Media watch**: one `MediaWatch` thread in `Engine`, with per-key reference counts across docs (two docs can watch the same file; one unwatching must not stop the other's watch). Events are routed to every doc that watches the key.
- **Network output**: `NetOut` send threads per doc are fine. The look-up thread budget (at most 8) is engine-wide, and the PB-10 output override becomes `Engine` state applied to every doc.
