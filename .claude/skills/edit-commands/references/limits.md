# Size limits

Rule for all limits: they apply to the **value being set only**. Load keeps an odd value and warns; undo, redo and a transaction's rollback put such a value back; other edits do not re-judge a value already in the cue. An edit may replace an over-limit value with one no longer than it (a longer one is refused). The same bound holds on every path that creates or replaces the data:
`AddCue`, `Paste`, `PasteClipboard` (any depth), `SetCueProperty`, `BatchSet`, and a `Kind` change. A refusal reaches the client as `command_refused`. Constants live in `validate.rs` (`MAX_NAME_SCALARS`, `MAX_CUE_NAME_BYTES`, `MAX_CUE_NOTES_BYTES`) and `network.rs` (`MAX_HOST_BYTES`).

| Field | Limit | Refusal | Load |
|---|---|---|---|
| Name of workspace, cue list, patch, network patch (add commands, renames) | 1024 Unicode scalar values (characters, not bytes, so any script gets the same room); no control character except newline (NUL, tab, CR, escape codes refused); empty allowed | `InvalidValue` | one `InvalidName` warning per field kind: `name`, `cue_lists.name`, `patches.name`, `network_patches.name`, `network_patches.host` |
| Network patch `host` | 253 bytes (DNS limit), no control character. Whether it can be addressed is not checked (a bad host makes a Network cue broken) | `InvalidValue` | `InvalidName` |
| Cue name | 1024 bytes (`MAX_CUE_NAME_BYTES`; 256 characters of any script at 4 bytes each; deliberately not raised with the names above) | `InvalidCue`, `TooLarge { field: "name" }` | `TooLarge` warning |
| Cue notes | 64 KiB (`MAX_CUE_NOTES_BYTES`, the bound on a Network string argument); free text, newlines allowed | `InvalidCue`, `TooLarge { field: "notes" }` | `TooLarge` warning |
| Network cue address | 64 KiB | `InvalidMessage(reason)`; on add/paste/kind change `InvalidCue` (`TooLarge`) | `TooLarge` warning; cue shows broken (`network_message_invalid`) |
| Network cue arguments | 1024 arguments; each string argument 64 KiB; no non-finite float (JSON cannot hold one) | as above | as above |
| OSC passcode (via command) | 1 to 32 ASCII digits; at most 16 rows | `InvalidValue(&'static str)` (the table is not changed); a bad index is `IndexOutOfRange` | a show file may hold other text; kept with a warning |
| Network patch port (new or changed value) | 1 to 65535 | `InvalidValue` | port 0 accepted at load |
| Patch outputs / `channel_count` | 1 to 128 (`MAX_CUE_OUTPUTS`) | `channel_count` edit: `InvalidValue`; `AddPatch` or outputs edit with 0 or over 128: `InvalidModel` carrying `PatchChannelCount`; a bad output index: `PatchOutputOutOfRange` | per `docs/file-format.md` |

Why:
- Newlines in names are allowed as in QLab 5. The 1024 scalar limit for names is our choice (QLab sets none we know of).
- The Network message bounds sit far above the send limits (512-byte address, 32 arguments, 4096 bytes per string, 8192 per message; `network::check_message`, skill `osc`), so normal typing is never refused. They only stop a hostile client filling the document through one command. Anything between the send limit and the bound is accepted and the cue shows broken.
- A malformed message or an unknown destination is never refused by the editor (QLab accepts it); the cue shows broken.
