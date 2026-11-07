---
name: wire-protocol
description: "Load when adding, removing or changing an engine-UI WebSocket message, request, runtime change, error code or limit (engine/crates/core/src/protocol, engine/fixtures/protocol, mac/openLab/Engine/ClientProtocol.swift and ServerProtocol.swift) or reviewing such a change."
---

# Changing the wire protocol

Behaviour, ordering and limits are in `docs/protocol.md` (read the sections for your message). Fields are defined by the Rust types in `engine/crates/core/src/protocol/` (`client.rs`, `server.rs`, `changes.rs`, `mod.rs`); the doc does not restate them.

## Same commit, always
1. Rust types in `openlab-core` (the source of truth).
2. Golden fixtures in `engine/fixtures/protocol/{client,server}/`: one file per message variant and per nested variant the client must decode. Regenerate: `UPDATE_PROTOCOL_FIXTURES=1 cargo test -p openlab-core --test protocol`. Without the variable the test compares byte for byte and fails on drift; it also fails if a variant has no sample. Review the fixture diff.
3. Swift `Codable` types: `mac/openLab/Engine/ClientProtocol.swift`, `ServerProtocol.swift`. `ProtocolFixtureTests` decodes every fixture and re-encodes client messages.
4. `docs/protocol.md`: behaviour only (ordering, scope, errors, limits). Do not add field tables.
5. Control-loop and server handling, plus a test over a real socket or the control-loop test harness.

## Version rule
- Unknown fields are ignored; unknown message types and nested variants are errors.
- Removing, renaming or re-typing a field, or changing its meaning, bumps `PROTOCOL_VERSION` (`protocol/mod.rs`). An added optional field does not.
- Bumping: update the constant and the line `PROTOCOL_VERSION is **N**` in `docs/protocol.md`. `scripts/check-repo.sh` (check `versions`) fails unless that exact line matches the constant, and fails on any `PROTOCOL_VERSION is/stays N` or `(now **N**` with another number. Write "stayed N" for past versions, never "stays N".
- Keep the section numbers (2, 3, 4) and the titles "Transport actions", "Opening and closing workspaces", "Pre-show check", "Show log entries and status", "Snapshot", "Test support": code comments and tests quote them.

## Must not break
- Hostile or oversized frames: checked by byte length before parsing; decode never panics. Add the case to the protocol fuzz body when you add a decoder path.
- `model_changed` for revision N precedes the `result` reporting N; deletes apply before upserts; revisions step by 1.
- A numeric field that can be NaN or infinite on output is sanitised to 0 by the encoder.
- Test-only requests (`advance`, `lose_device`, `restore_device`) never exist in a release build; `mac/scripts/verify-bundle.sh` checks.
- Touching `engine/fixtures/protocol/` or `engine/crates/core/src/protocol` triggers the mac PR checks and the real-engine app tests (`scripts/paths.sh`).

## Related
Skill `engine-ws-server` for server mechanics; `edit-commands` when the change is a model `Command` (commands ride inside the `command` message).
