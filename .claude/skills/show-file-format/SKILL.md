---
name: show-file-format
description: Load when changing or reviewing what is written to or read from a .openlab show file: model fields (engine/crates/core model.rs, cue.rs, show_file.rs, validate.rs), load-time validation and warnings, format_version, the migration chain, media path storage, or fixtures in engine/fixtures/show.
---

# Changing the show-file format

The contract is `docs/file-format.md` (conventions, §3 load/validation table, §4 workspace, §5 cues). The Rust model is the source of truth for field types; the doc states the stored shape and load behaviour.

## Same commit, always
1. Model change in `openlab-core` (`model.rs`, `cue.rs`) and loader/validation (`show_file.rs`, `validate.rs`).
2. `docs/file-format.md`: the field table in §4/§5, and a §3 row for every new refuse/warn/repair behaviour. Keep the numbering §4.2, §4.3, §5.1 (source comments cite them).
3. Edit the current-version fixture in place: `engine/fixtures/show/v<N>-example.openlab`, where N is `FORMAT_VERSION` in `show_file.rs`. It must stay loadable with every cue kind and group mode. Tests in `engine/crates/core/tests/show_file.rs`.
4. Keep `FORMAT_VERSION` where it is until v1.0.0 (CLAUDE.md "Contracts"). Add no migration step and no old-version fixture. A new required key also goes into the last existing migration step so older test shows still load.
5. If protocol messages carry the field, also follow skill `wire-protocol`. Swift reads cues and settings in `mac/openLab/Engine/Model.swift`.

## What check-repo parses
- `scripts/check-repo.sh` (check `versions`) reads the line `Status: format version **N**` in `docs/file-format.md` and requires N equal to `FORMAT_VERSION`. Keep that exact form.
- Touching `docs/file-format.md` or `engine/crates/core/src/show_file*` triggers the mac PR checks (`scripts/paths.sh`).

## Must not break
- Any input file, however hostile, loads, warns or is refused cleanly: never a panic, hang or unbounded allocation. Size and depth limits (`MAX_SHOW_FILE_BYTES`, parser depth) apply on every open path including recovery files.
- Refuse only what makes the show ambiguous (bad JSON, duplicate ids, nesting too deep, newer version). Everything else loads with a warning naming the cue, and the cue shows broken where it cannot run. Oversize names and text are kept so the show opens; commands refuse new oversize values.
- A limit or validation applies on every path that creates the data (load, command, paste, import) and is tested on each.
- Unknown keys are ignored; missing required keys are an error (except `settings.hotkeys` keys).
- Media paths are stored with forward slashes, relative only when literally inside the show folder (byte for byte after NFC). Matching rules and relink are in skills `media-files` and `persistence`.
- Output is deterministic: fixed field order, so saving twice gives identical bytes.
