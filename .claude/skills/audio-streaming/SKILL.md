---
name: audio-streaming
description: Load when changing or reviewing media decoding, probing, preload, disk streaming, resampling, loop cache, slices, loops, devamp or the waveform overview in engine/crates/audio (decode.rs, pcm.rs, stream.rs, plan.rs, resample.rs, waveform.rs) or damaged-file handling.
---

# Decoding, disk streaming, slices, loops and waveform (`openlab-audio`)

Everything here runs on disk, opener or I/O threads, never on the audio thread. The mixer side of a voice (rings, underrun, rejection, pause) is skill `audio-mixer`. The engine's use (priming policy, start limits, media catalog, stalled volumes) is skills `engine-control` and `media-files`.

## Must not break
- Decode, probe, preload and waveform never run on the audio or control thread.
- A damaged, truncated or hostile file never crashes, hangs or allocates by a size the file declares. A panic inside symphonia is contained (`decode::contain`): the cue breaks `Corrupt("decoder failed")` and the worker lives on.
- A damaged file plays: missing frames are silence of their length, so every later position, seek, loop and slice stays exact. Silence is played only for damage that is also reported; a file that ends early is `Truncated`, never padded.
- Seeking yields exactly the frames a decode from the start yields (AAC noise substitution is the one tolerance).
- A hung open (dead network share, stalled USB disk) never starves a stream that is already open, never takes the last opener, and never blocks the caller.
- A FIFO, socket, device or folder at a media path is refused at once (`NotAFile`), never opened for a blocking read.
- A stream's error is published only after its ring producer is dropped, so the voice is certain to play out what was written.
- The voice and the disk worker agree exactly on the slice table, loop boundaries, and whether a loop exit is used.
- No sample reaching the mixer is non-finite or beyond +-16.0.
- Memory is bounded per stream and engine-wide, allocated fallibly on the worker, and over-budget degrades (seek, refuse) rather than aborts.

## Decoding (AUD-04)
- `decode.rs` wraps symphonia 0.6. Formats: WAV and AIFF PCM, FLAC, ALAC (M4A), MP3, AAC (M4A), Ogg Vorbis (optional). 1 to 64 channels and 8000 to 384000 Hz; anything else is `Unsupported`.
- The 8000 Hz floor exists because rubato sizes its FFT chunk from the rate ratio: a 1 Hz file at 48 kHz asked for a 49-million-frame chunk (out-of-memory found by the decode fuzz target). `Pipeline::new` refuses resampling outside the same range.
- WAV/AIFF with 9 to 64 channels is read by the in-house `PcmSource`, because symphonia refuses most layouts that wide (details in `references/decode-details.md`).
- `probe(path)` / `probe_media(media)` return `Probed { info: MediaInfo { channels, sample_rate, frames, duration }, warnings }`.
- `frames` is exact on the playback timeline: a PCM header's length is verified by decoding the tail; every compressed file is decoded once in full and counted. That full decode also finds all damage, so it is known at cue load, not during the show.
- Samples are made safe: a non-finite sample (NaN, inf; possible in float files) becomes 0.0 and the rest clamp to +-16.0 (+24 dBFS, the mixer's gain ceiling), so one bad sample cannot poison a mix bus or the resampler.
- Typed errors (`MediaError`): `NotFound`, `NotAFile`, `Io`, `Unsupported`, `Corrupt`, `Truncated { expected, actual }` (a stream's file became shorter than its probe), `Changed` (no longer matches its probe), `BadSlices`, `TooManyStreams`, `Worker`, `TooLarge`.
- Every media open, probe, preload, waveform and disk stream goes through `open_regular` (`regular_file.rs`, shared with the engine's show-file reads). It opens non-blocking on Unix (Windows never waits) and checks the type on the opened handle, so a FIFO cannot hang a worker.
- `preload(media)` decodes a whole file into memory (`Preloaded`, AUD-05) at the file's rate. Budget `DEFAULT_PRELOAD_BUDGET` (512 MiB); `preload_within(media, max_bytes)` sets another. The declared length is checked before decoding and the decoded length while decoding (a small compressed file can expand far beyond what it declares). Over budget or an allocation the system refuses is `TooLarge { bytes, limit }`. The engine decides whether the cue then streams or is reported.

## Damaged files play, with warnings
`MediaWarning { kind, at, frames }`: positions are frames at the file's rate on the playback timeline; each warning once, at most 256 per file.

| Kind | Meaning |
|---|---|
| `Missing` | frames the container skipped (CRC failure, lost Ogg page, splice) or a packet that did not decode. Played as silence of their length. Damage in the first packets makes silence from position 0 |
| `Repeated` | frames repeating positions already played; dropped |
| `Truncated` | file has `frames` fewer than its header declares; plays to its real end at `at`. For lossy formats the missing frames may lie anywhere |
| `Unreadable` | container keeps failing (16 errors in a row) or timestamps jump by more than a minute; plays to `at` |
| `PrimingIgnored` | AAC-in-MP4 gapless info unusable; plays as decoded, starting with the encoder's priming |
| `SlowSeeks` | damaged file whose seeks must decode from the start; `Probed::preload_advised()` is then true |

- Only a file with nothing playable is an error (`Unsupported`, `Corrupt`, `Io`, `NotFound`).
- Damage while streaming is handled exactly as the probe handles it: `StreamHandle::warnings()` lists everything met (probe-reported damage reappears; anything else means the file changed after its probe). The stream reads nothing past the probed length.
- Seek policy for damaged files (`Probed::seeking`, passed in `StreamRequest`): `Direct` (undamaged, and truncated-only PCM, container seeks), `Indexed` (checkpoint hashes verify the seek), `FromStart` (decode forward). Why and how: `references/decode-details.md`.
- A disk worker does at most `step_frames` (`DEFAULT_STEP_FRAMES`) of seek decoding per step then serves its other streams, so one slow seek never starves them.
- Known limits: symphonia's FLAC reader stops at damage in a very short file (reported `Truncated`); an Ogg file whose damage defeats the length scan has no declared length, so audio lost at its end cannot be reported; in a damaged Vorbis file the page after a lost page plays in its place and silence comes a page later (playback, preload and seeks all agree on that timeline).

## Disk streaming
- `DiskStreamer::new(StreamerConfig)` starts disk workers (`DEFAULT_WORKERS`) that decode and resample each stream into its voice's ring, about `buffer_ms` (`DEFAULT_BUFFER_MS`, 2000) ahead, and opener threads (`DEFAULT_OPENERS`) that only open files.
- `prepare(StreamRequest { media, info, seeking, slices })` checks the slice table against `info` and the open-stream limit (`DEFAULT_MAX_STREAMS`), creates the ring, hands the stream to a worker, and **does no I/O**. It returns a `PreparedStream`; `into_voice(voice, cue_outputs)` makes the silent `VoiceStart` (with `slices` and `link`) and keeps the `StreamHandle`. Dropping the handle cancels the stream.
- **Primed** (`is_primed`): the first chunk (4096 frames, or the whole stream if shorter) is in the ring; the cue counts as loaded. `is_finished`: every frame of the plan is in the ring. The blocking `wait_*` helpers are for I/O threads and tests, never the control thread.
- Each worker step is bounded: one 4096-frame chunk, or `step_frames` of seek decoding, or one chunk of a loop exit's buffer. Streams take turns round-robin.
- **Resampling** (`resample.rs`): rubato's synchronous FFT resampler when file rate differs from device rate, pass-through otherwise. Device frame `p` is the signal at `p / device rate` whether reached by reading or by seeking (a seek restarts the resampler on a chunk boundary one chunk early and discards its delay). Length is exactly `round(frames * device rate / file rate)` (`MediaInfo::frames_at`).
- Preloaded media goes through the same resampler and ring, so it plays exactly as streaming does.
- Loops are unrolled into the ring ahead of time, so a loop's own seam never waits on the disk. Trims and start offsets are seeks.
- **Underrun** from a slow disk gives the mixer's underrun behaviour (silence, counted, resumes where it left off).
- **Failures.** A stream that cannot continue (read error, file shorter than or no longer matching its probe, worker panic) sets `StreamHandle::error` and lets go of its ring. Publication order is the invariant (see Must not break).
- Dropping the `DiskStreamer` tells workers and openers to stop and does not wait, so a worker or opener blocked on a stalled disk cannot hang the caller.

### Opener pool: isolating hung volumes
A file (`Media::Path`, `Media::Reader`) is opened on the opener pool, in order, by whichever opener is free, and the opened decoder is handed back to the stream's worker. Preloaded media is built by the worker (no I/O).
- A hung open holds one opener and its own stream, never a worker: open streams keep refilling.
- Opens are filed by volume (`volume_of`; the engine passes its probe gate's volumes, the media watch's `bucket_of`). One volume's opens hold at most `openers - 1` openers at once (all of them only with a single opener); a free opener takes the oldest job whose volume is under that cap.
- The cap is not 1 because `/` mixes the local disk with automounts: a cap of 1 would let one hung automount block every local open.
- Opens hung on several volumes can still hold every opener between them (one NAS exporting two shares). So when an open on a volume with nothing in flight finds no free opener, the streamer starts an extra opener at once, at most `MAX_EXTRA_OPENERS` beyond the pool (the same bound as the probe pool's `MAX_ABANDONED_WORKERS`).
- An extra only takes an open on a volume with nothing in flight, so opens queued on a dead volume never use up the extras (each dead volume holds at most one); it exits as soon as there is none. Spawn decisions are made under the queue lock and the spawns after releasing it.
- Past the bound no thread starts: an open on yet another volume waits for a free opener, and a started cue on it breaks after the engine's start limit (skill `engine-control`). Streams already open and preloaded cues are never affected.
- A stream whose open has not answered is neither primed nor failed. A stream dropped while its open waits in the queue is never opened.
- `StreamerConfig::before_open` (feature `test-support` only) runs on the opener just before a path is opened, so tests can stand in a stalled volume.

### Loop cache
- The first play of a loop entry (`Infinite`, or `Times(n)` with plays to come) that starts at its first frame is recorded as it goes into the ring: exactly the device-rate frames written, after decoding and resampling. Later plays are written from the recording instead of seeking back, several per worker step for short loops (up to one 4096-frame chunk).
- Why: tight loops cost a copy, and every play of a lossy loop is the same frames (an AAC or MP3 decoder restarted by a seek gives slightly different output each time). For lossless files the output is bit-identical to seeking (tested for WAV and FLAC; not proven for every rate pair).
- Damage met in the first play is reported once; later plays repeat its silence.
- Bounds: a play of at most `loop_cache_bytes` (`DEFAULT_LOOP_CACHE_BYTES`) per stream, one loop at a time (dropped when the cursor leaves the entry or a devamp cuts it), taken from the streamer-wide `cache_budget_bytes` (`DEFAULT_CACHE_BUDGET_BYTES`, for loop caches only) and allocated with `try_reserve_exact` on the disk worker.
- A loop over a bound, or when the budget or allocation fails, seeks once per play (the next play tries again).
- A loaded cue records its cache while its ring is first filled, before the voice starts.
- Report with `StreamHandle::cached_frames`/`cache_bytes` and `DiskStreamer::cache_bytes`.

### Loop exits
Leaving a loop early (devamp) is the one place the ring cannot be ahead: it is FIFO and full of loop plays that will not be heard.
- The worker decodes each loop exit's continuation (the slice table unrolled from the slice after the loop) for `exit_ms` (`DEFAULT_EXIT_MS`, 250 ms) into an exit buffer published on the voice's `StreamLink`; later plays of a loop within it are copied from its first play.
- At the exit the voice plays that buffer while it drops the stale plays from the ring; the worker refills the ring from where the buffer ends (`StreamHandle::last_resume` reports where, for diagnostics).
- The 250 ms gives the worker time to notice the freed ring (5 ms poll), seek (fresh demuxer and pre-roll, up to about 10 ms on long compressed files) and decode a first chunk, with up to one 8192-frame device block still taken ahead.

**Build order (prevents starvation and missed exits)**
- The first chunk goes into the ring before any exit is built, so a cue primes as fast as one without loops.
- After it, no frame of a loop is written to the ring until that loop's own exit is decided (built or refused). So a devamp of a loop the voice reached through the ring always finds its exit ready. Loops inside the first chunk are caught up on first.
- Exits of loops that an exit buffer reaches (the voice can be in them before the ring holds their frames) are built nearest first, whenever the ring is at least two chunks ahead of the voice (8192 frames, or half the ring if smaller). Building them therefore never starves the ring. A dense table (60 slices of 100 ms, every other looping) never underruns (tested).
- A devamp before exits are ready (within the first chunk of a cue starting in or just before a loop, or out of a loop reached through an exit buffer right after priming) takes the fallback.

**Memory**
- One buffer per distinct loop exit: `exit_ms` x channels x 4 bytes, capped per stream by `exit_bytes` (`DEFAULT_EXIT_BYTES`) and taken from the streamer-wide `exit_budget_bytes` (`DEFAULT_EXIT_BUDGET_BYTES`). This budget is separate from the loop caches' so optional cache memory never crowds out exits. Allocated fallibly on the disk worker.
- An exit put off because the budget was full is built when the stream is idle and the budget has room (nearest first among loops the voice can still reach). One over the per-stream cap, or whose allocation failed, stays refused.
- Exits are freed with the link when voice and stream are done. A voice start the mixer leaks (`leaked_returns`) keeps its link, so its exits' bytes stay reserved for the run; that only reduces other streams' room.

**Fallback** (exit refused, put off for budget, or not yet built): the devamp is carried out the same way, but at the boundary the voice has nothing to play until the worker has cut and refilled the ring behind the stale frames. That is an underrun (silence, counted) of about one worker poll plus a seek and a chunk; playback then continues exactly where it was.
- Whether the voice uses an exit is decided when the devamp is accepted and sent with the cut, so voice and worker always agree even if the exit appears in between.

## Slices, loops and devamp (AUD-11, AUD-12, CT-16)
`plan(range, marks, plays, start)` builds a slice table in device frames: the in/out range cut at `marks`, `plays[i]` (`Times(n >= 1)` or `Infinite`) for slice `i` (empty = each once), starting at `start` inside the range.
- A start inside a looping slice splits it into a one-play entry and an entry for the remaining plays, with the same `index`. Bad input is a `PlanError`. `total_frames` is `None` if anything loops forever.
- The same table drives the disk stream (unrolls it into the ring) and the voice (counts frames to find boundaries), so both agree exactly.

**Minimum loop length.** Every play of a loop is one `LoopReached` event and one boundary for the voice, so a loop of a few frames would flood the 1024-entry event queue and keep the mixer at its boundaries.
- The engine builds tables with `plan_at_rate(range, marks, plays, start, device_rate)`: `plan` plus `check_loop_lengths`. Every entry that plays a slice again (more than one play, or the rest of a slice split by a start offset) must be
  at least `min_loop_frames(device_rate)` = `MIN_LOOP_MS` (50 ms) in whole frames, rounded down, less one frame of slack for slice times rounded to frames (2399 at 48 kHz). Otherwise `PlanError::LoopTooShort { slice, frames, min }`.
  Exactly the minimum is accepted; slices that play once may be any length.
- 50 ms matches the floor of core's `MIN_SLICE_SECONDS` and keeps 128 looping voices to about 13 loop events per 5 ms control tick.
- `plan`, the disk stream and the mixer still accept shorter loops (existing callers and tests use them); with the loop cache even a one-frame loop costs one copy per chunk, not a seek per play, but its events are unbounded. So engine paths must use `plan_at_rate`.

**Events** (exact sample `at`):
| Event | When |
|---|---|
| `LoopReached { slice, count, at }` | a play of `slice` ended and it plays again from `at`; `count` plays done |
| `SliceEnded { slice, at }` | the slice's last play ended; the next slice, if any, starts on `at` |
| `DevampScheduled { at }` | a devamp was accepted, or the same devamp (same `stop`) was sent again while pending; `at` is where the voice leaves the loop unless an underrun intervenes |
| `MoveEnded { at, moves }` | the voice plays through its current patch only from `at`; `moves` counts `MoveVoice`s taken since start (wrapping) so the control thread can tell a move still on its way |
| `DevampIgnored { at }` | a devamp that cannot be carried out: no such voice playing, no slice table, a different devamp (other `stop`) pending, one needing a cut while an earlier cut still waits to be sent, or no `link` when needed. Playback unchanged |

Voices without a slice table report only `VoiceFinished`.

**Devamp** (`CommandKind::Devamp { voice, stop }`, CT-16) leaves the current loop at the end of the play under way when the command applies (a devamp on a loop boundary sample exits at the end of the play that starts there).
- **Non-looping slice (bridge):** a devamp while a non-looping slice plays leaves at that slice's end, i.e. has no effect, and the next loop keeps looping. Assumed behaviour: QLab's manual is silent (see skill `qlab-behaviour`).
- **Idempotent.** A devamp equal to the pending one (same `stop`) changes nothing (no second devamp, no new cut) and is answered with `DevampScheduled` at the pending boundary again. The cue engine's re-send after a resume relies on this. A different `stop` while one is pending is `DevampIgnored`.
- `stop: true` ends the voice there (`SliceEnded`, then `VoiceFinished { reason: OutPoint }`).
- `stop: false` continues with the next slice. If more plays of the loop may already be in the ring, the voice publishes the boundary on its `StreamLink` and plays the loop exit's buffer at the boundary; the worker stops unrolling there,
  writes the continuation from the end of the exit buffer and publishes where it starts; the voice skips the stale frames in between. This holds however full the ring is and however close the devamp comes (tested at 1 frame, 85 ms and 4096 frames before). A devamp before the ring reaches the boundary skips nothing.
- To start the next cue when the loop ends, the engine uses `DevampScheduled.at` (schedule the start in the block containing it) or reacts to `SliceEnded`.
- If the disk worker cannot place the boundary in the table, the stream fails (`BadSlices`) and the voice ends, rather than waiting forever.

**A devamp while an earlier devamp's exit buffer plays** is carried out like any other, at the end of the play under way (vamp A, a bridge shorter than the exit buffer, vamp C: a devamp in C leaves C at the end of that play even if the boundary falls inside A's buffer).
- Cuts are dated in voice frames (frames of the plan played, exit buffers included); only the disk stream maps them to ring positions, so the voice needs no ring position.
- The link carries one cut at a time. A devamp arriving before the disk stream answered the previous cut (normally within one 5 ms poll of the previous boundary) is accepted and answered at once, and its cut is sent when the answer is in, even if the voice has passed its boundary meanwhile (it then plays the new exit buffer and waits for that answer too).
- Only one cut waits: a further devamp that needs a cut before then is `DevampIgnored`, because a dropped cut would make the disk stream map the next one from the wrong place.
- Second line of defence: the disk stream fails the stream (`BadSlices`, the voice ends) for a cut that is not on the end of a play of a loop, instead of cutting anywhere else. A randomized test drives tables and devamp sequences with the worker held at random against a reference model; keep it passing.

**Integrated fade-out with slices** follows the planned end: it ends on the last frame of the table's last play.
- A voice heading into an infinite loop has no known end, so no fade-out applies until a devamp continues it into a finite part.
- If the devamp leaves fewer frames than the fade-out's length, the fade restarts from the gain the last frame before the jump had and reaches exactly 0.0 on the new last frame, same curve: the gain never jumps.
- A devamp stop ends at the loop boundary without the fade-out.

## Waveform overview (AUD-14)
`waveform.rs` builds the data the waveform editor draws (drawing and drag editing are in the mac app). It runs on a worker thread, never the control or audio thread.
- **Format.** `Waveform { sample_rate, frames, channels, levels }`. Level 0 holds one `Peak { min, max }` per `BASE_FRAMES` (256) frames per channel; each further level merges neighbouring buckets by 2 (a zoom step never loses more than 2x resolution) until one bucket covers the file; the last bucket of a level may cover fewer frames. Peaks are `i16` (full scale 32767). Samples are clamped to +-1 first (a float file's overs show as full scale) and NaN/inf count as 0.
- **Positions are file frames at the file's own rate**, the unit trims and slices use in the show file (seconds from the start of the file; `plan` takes frames), so `seconds x sample_rate` is the frame to ask for.
- `WaveformBuilder::new(rate, channels, expected_frames)` / `push(interleaved)` / `finish()` is pure; the result does not depend on chunking. It reserves for `expected_frames` once and ignores frames beyond them, so a file longer than its probe cannot grow it.
- `build_waveform(media, &info, &cancel)` decodes with the regular decoder (no second path) in 4096-frame reads. `info` is the probe from cue load (not probed twice). `cancel` is checked before every read and gives `WaveformError::Cancelled`.
- Damaged media still gets a waveform: `WaveformResult { waveform, warnings }` (missing frames are silence in it). Fewer frames than probed gives a waveform of the frames present plus `Truncated`; more is read only up to the probed length. Channels or rate differing from `info` is `MediaError::Changed`; unplayable is `WaveformError::Media(..)`; a decoder panic is `Corrupt`.
- `Waveform::range(channel, start, end, buckets)` returns `buckets` min/max pairs over `start..end` from the coarsest level that still has a source bucket per output bucket (level 0 if even that is too coarse: output buckets repeat source buckets, so the editor reads samples directly at finest zoom). `end` is clamped to the file; edges round outwards by up to one source bucket. Empty for an unknown channel, `buckets == 0`, or an empty or inverted range; never more than `MAX_RANGE_BUCKETS`.

## Reference files
- `references/decode-details.md`: read when touching `pcm.rs`, `mp4_priming.rs`, `mp4_check.rs`, `riff_check.rs`, the seek index or `Seeking` policy, position/priming rules, or changing the symphonia version.
