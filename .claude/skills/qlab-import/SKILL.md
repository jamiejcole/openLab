---
name: qlab-import
description: "Load when changing or reviewing the new workspace from a QLab 5 workspace file (.qlab5): the keyed-archive reader and converter (core src/qlab/), the inspect_qlab and import_qlab requests (engine control/qlab.rs), its fixtures, fuzz target or tests."
---

# New workspace from a QLab 5 workspace file

The operator picks a `.qlab5` file; openLab makes a **new, unsaved** workspace from it. There is no import into an open workspace. The file is read, never written, and never through OSC or a running QLab (owner, 2026-11-26).

## Where it lives
- `openlab-core` `src/qlab/`: pure, bytes in. `plist.rs` reads binary property lists lazily (one object at a time by index, so memory is the input's size); `archive.rs` reads `NSKeyedArchiver` archives over it; `convert.rs` maps QLab to openLab; `report.rs` holds the report and the choice types the protocol carries; `mod.rs` holds `read_qlab`, `QlabImport::finish` and `apply_qlab_choices`.
- Engine `control/qlab.rs`: the document worker reads the file (size-capped, regular files only, as `open` does), looks for media, and the control thread stores the device choices when the doc is installed.
- Protocol: `inspect_qlab` and `import_qlab`, `qlab_report`, error codes `not_qlab_workspace`, `qlab_too_old`, `qlab_too_new` (docs/protocol.md, "Opening and closing workspaces").
- The field-by-field mapping, the enum values and the fixture that shows each: `references/mapping.md`.

## Must not break
- **Any input is converted or refused, never a panic or hang.** Every offset, length and reference is checked; no unchecked indexing. Limits:
  - `MAX_QLAB_FILE_BYTES` (the show-file limit); `MAX_STRING_BYTES` per string; `MAX_QLAB_CUES` cues; `MAX_GROUP_DEPTH` nesting (deeper cues are left out with a note).
  - `MAX_READS` object reads over the whole import: a file that makes the reader revisit shared objects runs out of reads (`too complex`).
  - `MAX_ARCHIVES` (4) archives opened: the file and the cue-list archive held in it as data.
- Refusals: not a property list, not a keyed archive or no cue lists: `not_qlab_workspace`; a version below 5, or cue lists with no version: `qlab_too_old`; a required minimum version above 5: `qlab_too_new`; damaged data or a limit: `bad_show_file`. A cue whose own data cannot be read becomes a placeholder (reason `damaged`); only limits stop the whole read.
- **Every id is new** (from the caller: core draws no randomness); QLab unique ids are kept beside them (`QlabImport::cues`, `patches`, `routes`) for targets, choices and the report. A cue id seen twice (a cycle or a shared cue) is left out the second time with a note.
- Targets resolve once every cue is converted; one that did not convert, or that a delete removed, becomes none (`apply_qlab_choices`). A Target cue's new target is found by its cue number.
- The converted workspace goes through `repair` as a load does; its warnings are the new doc's `load_repair` notices. The fuzz target asserts that whatever converts saves and loads back.
- OSC access passcodes are never imported: the workspace gets a new passcode as `new` gives one (`OscAccess::new_workspace`, seeded from the id source).
- The new doc is untitled and dirty. Media resolve next to the `.qlab5` file (QLab's relative path), then at QLab's last known path, else the path next to the file is kept and shows missing (relink as usual). They are stored absolute: an untitled workspace has no folder.

## The operator's choices (`import_qlab`)
- Each unsupported cue (`qlab_report.unsupported`) is deleted or kept as a placeholder; one not named gets `other_cues` (absent: placeholder), so the apply-to-all buttons need no list (which a big show could push past the client frame limit). A placeholder is a red Memo keeping number, name, notes, pre-wait, post-wait and continue mode; with no name it is called "QLab <type> cue (not converted)". Deleting a group's child leaves the group.
- A cart (a cue list of group mode 5): its placeholder is a cue list of its cues, converted as any list's (grid positions are lost); deleting it removes the list and its cues. The workspace keeps at least one list.
- Audio patches: a listed patch with a mapping is stored for this machine as `set_patch_mapping` stores one; listed without one it is stored cleared (left unmapped, so the default mapping does not map it); not listed it is mapped as any new workspace's patch.
- A mapping this machine cannot use when the doc is installed (the device gone since) is stored cleared and logged.
- Video outputs: a listed output with a display (a `list_displays` id) is stored as `set_route_display` stores one; anything else has none. A display not listed is logged.
- The QLab device's id (Core Audio UID) is in the report; `list_devices` gives names only, so the app matches by name.

## Fixtures and tests
- QLab-saved workspaces are **never committed** (QLab may store identifiers of the Mac that saved them; owner, 2026-11-26). They live in the ignored `engine/fixtures/qlab/<name>/` (the `.qlab5` and its `audio/`, `video/` media) on the machine that made them; the main checkout keeps a copy.
- CI tests need no QLab file: garbage, every truncation and corruption of a small archive shaped like a workspace (the fuzz seed `engine/fuzz/qlab-seeds/`, written by a script, not by QLab), counts outside the file, the size limit, and the choices on an openLab workspace (`engine/crates/core/tests/qlab.rs`); a non-QLab file refused through the engine (`tests/control/docs.rs`).
- Converter correctness is proven by the local tests, `#[ignore]`d with the reason "needs the local QLab-saved fixtures": one per fixture in core, and the engine's import (new unsaved doc, media found next to the file, device choices stored). They fail, never pass, when the folder is missing. Run them before a merge on a Mac that has the fixtures: `cargo test -p openlab-core --test qlab -- --ignored` and `cargo test -p openlab-engine --test control qlab -- --ignored` (from `engine/`).
- Fuzz target `qlab` (`make fuzz`; seeds `engine/fuzz/qlab-seeds/`).

## Remaking the fixtures
QLab 5.6.5 on the Mac, unlicensed. QLab has no OSC to make or save-as a workspace, and some fields (hotkeys, wall clock, load time, fade shapes and domains, extra patches) have no OSC address, so:
1. Media: `python3 scripts/qlab-fixture-media.py /tmp/qlfx/<name>` (tones and an image of our own).
2. Write a minimal keyed archive at `/tmp/qlfx/<name>/<name>.qlab5` (not under the home folder, so no user name is stored). Its root dictionary holds `workspaceName`, `uniqueID`, `QLabShortVersionString`, `QLabBuildNumber`, `QLabRequireMinimumVersion` (a `QLabVersion` object, not a dictionary), and:
   - `settings` holding only `OSC Access`, with a no-passcode entry of mask 15 and `allowRemoteConnections` true (else QLab answers nothing, or `denied`);
   - `cueLists`: a nested archive whose root is a `GroupCue` of mode 3 holding one `GroupCue` of mode 0.
3. `open -a QLab` it, then drive it over OSC on UDP 53000 (replies to 53001; send `/alwaysReply 1`): `/workspace/{id}/new {type}` makes a cue after the selected one, `/workspace/{id}/cue_id/{id}/{property} value` sets a property, `/workspace/{id}/move/{cue} {index} {parent}` nests it, `/workspace/{id}/save` saves. `/new list` and `/new cart` stop QLab with an alert (unlicensed): make extra lists by editing the file.
4. For fields with no OSC: edit the saved archive's objects directly (Python `plistlib`; fields QLab stores as objects must stay objects), then open it in QLab again and `/save`, so the fixture is what QLab makes of it. Point extra audio patches at a device this Mac lacks: QLab hangs opening a loopback device.
5. Scrub: blank every `F53Alias` `bookmark` (it names the volume), replace this Mac's audio device names, zero the display vendor, model and serial numbers; check every string in both archives. Open the scrubbed copy in QLab once (without saving) to see it still loads and finds its media.
6. When QLab shows an alert, `/usr/bin/log show --predicate 'process == "QLab"'` names the exception.
