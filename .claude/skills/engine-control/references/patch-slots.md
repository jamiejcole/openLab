# Patch slots, default mapping, set-mapping

## Slots (`Patches`, `control/patches/`)
- A patch's mixer matrix (slot) is allocated by `(DocId, PatchId)`, not by index in `Workspace::patches`, from one free list for all docs and devices (least recently freed first). Slot numbers are the same in every device's mixer; a slot's gains are sent to the mixer of its patch's device (`slot_output`), and a slot never sent to on a mixer is identity there. Every matrix is the mixers' cue outputs (128) by their device outputs (`Patches::new`); only entries that change are sent.
  A slot belongs to one doc's patch at a time. So inserting, removing, moving a patch (and undo/redo) is allowed while voices play and never re-routes or silences one.
- A voice keeps the slot its `VoiceStart` named until it ends, except that **a playing cue edited to name another patch follows it** (`Voices::follow_patches`, after every `sync_patches`): its voice (started, paused or stopping; not a preview) gets `MoveVoice` to the new slot, dated no earlier than its start/resume, and crossfades over the de-zipper.
  Outputs the new patch adds get their levels first (`Ramp`s, the model's with crosspoints, except a level a Fade cue set). No free slot for the new patch: the voice stays where it is until a later sync.
- The slot a voice left stays in use (`VoiceRec::former`) until the mixer reports every sent move settled (`MoveEnded.moves` equals the `MoveVoice`s sent) or the voice ends; a lost report only keeps the slot until then. `Voices::slots_in_use` lists slots of every voice the mixer may still hold (previews and orphans included).
- Rules applied on every `sync_patches`:
  - A patch keeps its slot while it exists. A new patch takes the least recently freed slot (at open, patch `i` gets slot `i`); none free means `unmapped` plus a `mixer_patches_exhausted` warning until one frees.
  - **A slot just handed to a patch takes its gains at once (`SnapPatch`)** instead of ramping from what it held, so a cue started or moved onto it that sample is heard only on its own patch's outputs.
  - A removed patch's slot is **retired** while a voice uses it: it keeps its routing and follows no other edit; when unused it is zeroed and freed (`reclaim_slots`, each tick while any is retired, then syncs again). An undo that restores the same id and output count takes the retired slot back. A slot is never handed out while a voice may play through it.
  - **Resize** (channel count or outputs) or **another device** under a playing voice: the voice keeps its layout and device (old slot retired) and the patch moves to a fresh slot, applying from the next start. With no voice on the slot the change is in place; a slot moving to another device is zeroed on the one it leaves.
  - A voice never moves between mixers: `follow_patches` leaves a voice whose cue's new patch is on another device where it is.
  While the patch exists, every sync rebuilds the retired slot's first `min(old, new)` outputs from the current master, levels, mutes, polarity and device mapping (`Patches::retired_base`), so a mute or master pull-down still reaches the playing cue; outputs the patch no longer has keep their last gains. With no voice on the slot the resize is in place. This is openLab's rule (skill `qlab-behaviour` Q226).
  - Level, mute, polarity, master and mapping edits apply in place, so to voices already playing.
- A closed doc's slots are zeroed and freed at once; a doc whose patches waited for a slot syncs on the next wake.

## Default mapping
Before each patch resolution (`sync_patches`: start, doc opened, edits like AddPatch, device back), every patch with **no mapping entry** on this machine gets, when the primary device plays, a mapping to its outputs 1..N in `config.machine`. The settings are written by the settings writer (a failed write is a `warn`, not the control thread) with an `info` log.
- **A patch with an entry is never touched** (an operator's mapping is never overwritten).
- A device with fewer outputs than the patch needs leaves it **unmapped** (silent, cues broken), never partly mapped; one `warn` per patch and device; a later sync with a bigger or other device maps it.
- No playable device (outage, none listed): nothing is written; patches stay `Unmapped` until a sync with a device.
- A patch the operator cleared has an explicit empty entry, so it counts as having an entry.

## `Request::SetPatchMapping { patch, mapping? }` (`Control::set_patch_mapping`)
Wire shape and errors: `docs/protocol.md`.
- Validated first against `list_devices` (device exists; one channel per patch output; each below the device's output count) and the doc's patches; a refusal (`not_found`, `bad_request`) changes and writes nothing.
- Machine settings are engine-wide, so the entry is keyed by **workspace id and patch id**, as the default mapping stores it (two open docs with one workspace id share it). Written like the default mapping.
- `mapping` null **clears**: entry becomes `{"device": "", "channels": []}` (patch `Unmapped`, default mapping never re-applies). A clear first **pauses every Audio cue playing (running or fading) through that patch** in each open doc with that workspace id (`DocCx::pause_cues_on`, an ordinary Pause, as QLab 5.5.3 does when a cue's device is set to none); mapping a device again does not resume them. Then every open doc re-resolves patches (`sync_patches`).
- A mapping to other channels of the same device moves a playing cue to the new outputs in place (the slot follows the mapping). A mapping to another device does not move it: the voice cannot leave its device's mixer, so the slot is retired and the cue plays on where it started; the patch's next start plays on the new device (skill `qlab-behaviour` Q257). Idle cues show broken or playable from the new resolution.
- A mapping to any device `list_devices` names is accepted. One the engine does not play on yet opens for it (skill `devices`, several devices); the patch is `device_missing` until it plays.
- A change that cannot be kept (settings folder configured but `settings.json` unreadable at start, so the engine leaves the file alone; or the settings writer not running) is refused with `io` and changes nothing. A later disk write failure is only a `warn`.
