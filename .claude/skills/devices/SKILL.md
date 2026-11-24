---
name: devices
description: "Load when changing or reviewing audio devices: listing, negotiation, callback, loss and reopen, stall watchdog, xruns, engine load, default patch mapping (audio/src/device.rs, output.rs, negotiate.rs; engine control/devices.rs, virtual_device.rs)."
---

# Audio devices

Two layers. **Mechanism** is in `openlab-audio`: `negotiate.rs` (pure), `output.rs` (pure: `OutputHost`, `DeviceMonitor`, `ErrorReporter`), `device.rs` (the only file that uses cpal, behind cargo feature `device`, on by default). 
- The fuzz crate builds `openlab-audio` with `default-features = false`, so it needs no ALSA headers.

**Policy** is in the engine: `control/devices.rs` (`devices::Device`, the supervisor), plus the device threads; `control/outputs.rs` is the table of open devices (AUD-02).
- The engine plays on several devices at once, each patch on the device its machine mapping names, shared by every open doc. Output 0, the **primary**, is the device opened at start; another device opens when a patch is mapped to it (see "Several devices").

## Must not break

- **A lost device stops every cue on it at once and nothing resumes by itself.** A 3 s and a 20 s outage behave the same. Only the operator's GO restarts the show. The other devices play on.
- **When the last device that plays is lost, nothing scheduled before the loss may fire after it**: pre-wait, post-wait, auto-continue and auto-follow of every active run are cancelled, not only those of audio cues. While another device plays, waits run on.
- **A stall is a loss only in the second stage** (or through the intermittent window rule). A one-stage 500 ms loss turns a recoverable dropout into a stopped show with every cue broken. A false positive costs the whole show; a late true positive costs seconds of an already silent show.
- **Only silence the supervisor actually watched counts.** A frozen process, a starved control thread or a sleeping Mac is never a stall.
- **The control thread never calls the device layer** and a GO never waits for a device (list, open, close run on device threads).
- **The stream never follows the system default**, and the engine never switches a stream's device while running. A patch on the system output follows it by moving its cues to another stream (below).
- **The mixer is never lost**: not on a failed open, not on a close, not on a hung device (it is handed back, or the failure is reported as unrecoverable).
- **The audio callback allocates, locks and logs nothing.** The error callback is one or two atomic writes.

## Listing and negotiation (`negotiate.rs`, pure)

- Zero devices is a valid answer. A `DeviceInfo` has a stable `id` (cpal `DeviceId`, `"host:id"`, used to open and reopen), `name` (what settings store and `set_output_device` takes), `is_default`, `configs`.
- **Rate** must be supported exactly (the mixer's rate is fixed when built); otherwise `NegotiateError::SampleRate` and the engine decides whether to rebuild the mixer.
- **Channels:** the exact count if offered; else the smallest larger count (extra device channels silent); else the largest smaller count. With fewer channels, mixer outputs `0..mapped` reach the device and the rest are not heard: `outputs_dropped` is reported so the engine warns.
- At most 64 mixer outputs are used; a device with more channels opens at its own count and channels above 64 stay silent.
- **Format** at that channel count: f32, f64, i32, i16, u16. Others (i24, u8, DSD) are skipped; none usable gives `NoConfigs`.
- **Buffer:** the requested size if in range, else the nearest end. The engine asks each device for its size from machine settings (`set_buffer_size`, 16 to 1024 frames, default 512; AUD-21) and logs a `warn` when the stream runs at another (`Engine::check_buffer`). A host that cannot report a range gets the requested size, and if it refuses, the host default.
- `open_output(mixer, &OpenRequest)` negotiates, builds and starts the stream. Failure is an `OpenError` with a `DeviceError` kind (NotFound, Unavailable, Busy, PermissionDenied, UnsupportedConfig, `Negotiate(..)`, Backend); `OpenError::take_mixer()` returns the mixer. A default device whose id the host cannot report fails with Backend rather than storing an empty id.

## The stream never follows the system default

- `device_id: None` means the default at the moment of opening. It is resolved to a concrete id and opened by id (CoreAudio: an AUHAL unit with disconnect reporting, not the default-output unit); `OutputStream::device_id()` reports it.
- Why: cpal's default-device stream is rerouted by CoreAudio in place and invalidated by WASAPI. A show could then move to headphones that connect mid-show while `negotiated()` and `outputs_dropped()` describe the old device.
- A reroute the host reports anyway is `Rerouted`, which sets `needs_reopen()`.

## The callback (`OutputHost::render`)

- The cpal data callback calls `render(buf, Some(CallbackTime))`; tests and drivers call the same function with their own buffers.
- f32 buffers with the mixer's channel count render in place. Everything else goes through a preallocated scratch of `SCRATCH_FRAMES` frames x mixer outputs; larger callbacks are rendered in pieces, so nothing allocates. Conversion clamps to +-1 and NaN becomes 0.
- A buffer that is not whole frames gets its tail silenced and counts `bad_buffer`; so does a buffer in a format the stream was not opened with.
- The body runs in `catch_unwind` on top of the mixer's own guard. A panic outside the mixer leaves the buffer silent and counts `callback_faults`.
- **Engine load:** `render` reads `Instant::now()` before and after the body, inside the allocation guard and a `catch_unwind`, and keeps the worst `elapsed / block duration` in one `AtomicU64` (`fetch_max`, parts per million plus one so 0 means "nothing measured"; integer maths).
- `DeviceMonitor::take_engine_load()` swaps it to 0 and returns the fraction (1.0 = the callback took its whole block), or `None` when no callback ran. Single consumer: the engine's sound health watch, every tick (`Engine::sound_health`); it keeps the worst for the next `status` and logs high load (skill `engine-lifecycle`, diagnostics).
- This clock read is the one sanctioned exception to "no syscalls" in the callback: it neither allocates nor locks and normally does not enter the kernel (commpage on macOS, TSC on Windows, vDSO on Linux). The CPU budget test reports its cost.
- **Allocation guard:** debug builds run the body inside `assert_no_alloc` with `warn_debug`, so a violation is counted (`DeviceMonitor::alloc_violations()`) rather than aborting; a debug build survives a mixer panic, whose unwinding allocates. The engine binary installs `AllocDisabler` as global allocator in debug builds (`main.rs`); without it detection is silently off.
- The control loop sums violations over every stream each tick and reports a rise as an `error` log and a `realtime_allocation` warning. Release skips the wrapper.

## Errors and loss reports

`ErrorReporter::report` runs on a host thread, sometimes the audio thread: one or two atomic writes, never allocates (cpal's error value may free its string when dropped).

| cpal kind | `HostError` | Effect |
|---|---|---|
| DeviceNotAvailable | `Lost` | counted; sticky `needs_reopen()` |
| StreamInvalidated | `Invalidated` | counted; `needs_reopen()` |
| DeviceChanged | `Rerouted` | counted; `needs_reopen()` (negotiated config is stale) |
| RealtimeDenied | `RealtimeDenied` | counted only |
| Xrun | `Xrun` | mixer `xruns` and `host_xruns()`, not an error count |
| anything else | `Other` | counted only |

- `DeviceMonitor::take_errors()` reads and resets; **single consumer** (the control loop's device supervisor). A second poller steals reports. `needs_reopen()` and the other getters reset nothing.
- Some hosts just stop calling back on unplug. `callbacks()` and `samples_rendered` standing still show it; the stall watchdog decides what it means.
- **Xruns:** `xruns()` is monotonic. Voice underruns are not xruns (`underrun_frames`). A callback starting more than 1.5 x the previous buffer's duration after the previous one counts one xrun (mixer `xruns` and `gap_xruns()`); 1.5x absorbs scheduling jitter and a missed buffer always exceeds it.
- An output's xruns rising are told to the operator as dropouts: an `audio_dropouts` warning naming the device, with a count, and show-log lines, rate-limited to one per ten seconds and cleared after ten seconds without one (skill `engine-lifecycle`, diagnostics). Nothing stops.
- A gap right after a host-reported xrun is not counted again (a report arriving only after the next callback can still double count). A clock stepping back re-baselines without counting; callbacks without timestamps are not checked.
- **Latency:** `OutputStream::latency()`: `buffer_frames` (host buffer, else last callback size, else the requested size) and `device_seconds` (playback minus callback time from the last callback's timestamps, when usable; CoreAudio includes device latency and safety offset).
- **Clock anchor** (`output.rs`, video follows it): the callback, when its `CallbackTime` has a known playback time and the host holds the mixer, writes `ClockAnchor { sample, host_ns }` (the mixer sample its buffer starts on, the host time that sample reaches the output). The writer is the audio thread only: a sequence lock of three atomics (odd sequence while writing, then sample and time, then even), so no allocation, lock or syscall.
- `DeviceMonitor::clock_anchor()` reads it from any thread with at most `ANCHOR_READ_TRIES` (16) tries and never spins longer. `None` before the first timed callback, and when every try met a write in progress (ask again next tick). An unknown playback time (`u64::MAX`) leaves the last anchor in place.

## Mixer handoff

- The cpal closure owns the `OutputHost`, which owns the mixer and scratch. Dropping the host pushes both into a one-slot `rtrb` queue; no allocation, free or block (tested under `assert_no_alloc`). The push cannot fail; if it ever did the mixer would be leaked and counted in `leaked_mixers`.
- `OutputStream::close()` drops the stream, then takes the mixer from the slot with voices, queues, clock and counters intact. Dropping `OutputStream` without `close` frees the mixer on the dropping thread, never the audio thread.
- **`MixerPending` is expected on macOS device loss, not an error.** cpal's CoreAudio disconnect thread holds the stream briefly while delivering `DeviceNotAvailable`, so the AudioUnit (and the handoff) drops on that thread a moment later. The engine polls `MixerPending::try_take` every tick until the mixer arrives.
- **While no device plays, the master clock is frozen** and every sample-time deadline waits with it. After the last device's loss nothing scheduled is left to wait on it; it only matters for what the operator starts during the outage (a memo with a wait stalls until a device returns). While the primary is away and another device plays, it runs on the wall clock (see "Several devices").
- Reopen backoff `reopen_delay_ms(attempt)`: 250, 500, 1000, 2000, then 4000 ms. The engine owns the clock, attempts and user warning.

## Engine device policy (`control/devices.rs`)

**Threads.** `openlab-device` owns the device layer (`DeviceLayer`: `CpalDevices`, the virtual device in test-support builds, or a stand-in in tests) and the open stream, because a cpal stream may not leave the thread that opened it and CoreAudio can block in open and close. It runs `Open { mixer }` and `Close` in order. `openlab-device-list` reads the list, so a hung open or close cannot stop hot-plug being seen. Both answer through `Inbox::Device`.
- A panic in the layer is caught on its thread and answered as a failure; a panic during open or close loses the mixer (`device_unrecoverable`). At most one list read and one open or close in flight. At loop end the device thread drops the stream and mixer on its own side.
- The first open (`devices::start`) is waited for at most `FIRST_OPEN_TIMEOUT`; then the engine stops with "no answer from the device".
- `host::run` opens the primary: the device chosen by `set_output_device` (machine settings `output_device`; `host::choose_device`), else the default, by concrete id. A chosen device that is not listed falls back to the default with a `warn`. A new choice applies at next start.
- Only the primary's `Device` has a list thread; its reads are handed to every other device's supervisor, which follow them (loss by leaving the list, return by coming back). Device-thread answers are tagged with their output (`Inbox::Device(key, ..)`).

**Supervision** (`devices::Device`, every tick, wall clock injected as `WallClock`):
- **List** every `DEVICE_POLL`; the last answer serves `list_devices`. A change re-resolves the patches.
- **Loss** is `DeviceMonitor::needs_reopen()`, or the stream's device gone from the list: missing from two reads in a row, or from one read while the stream has not called back since the previous read (one read can miss a device busy reconfiguring; cpal omits a device whose configs cannot be read at that moment). Other host errors are logged `warn`.
- **Close and reopen:** on loss the stream closes on the device thread; the mixer returns at once or as `MixerPending`. Reopens on the same id follow `reopen_delay_ms`, indexed by opens since the stream last stayed up `STABLE_UP`, so a device that keeps dropping right after each reopen backs off further. A failure logs the next delay. The device returning to the list (absent from the previous read) makes the next attempt immediate. A reopened stream continues the mixer as it was.
- **Another buffer size after a reopen:** lookahead (`CueEngine::set_lookahead`), the `now` offset and the wake rule follow the new block, so every deadline is committed before the callback that renders it. `now` never goes back. A callback already under way at reopen can still miss events of its first block (the size is known only once the stream runs).
- **Job watchdog** (`DEVICE_JOB_TIMEOUT`, wall clock): an open or close unfinished, or a mixer the host has not handed back, makes the device unrecoverable: `error` log and the `device_unrecoverable` warning naming what is stuck and that openLab must be restarted. A late answer still counts (warning goes, `info` log). A list read that never finishes is logged once and listed as `device_list_stuck`.

## Stall watchdog (two stages)

For a stream that stops calling back with no host error while its device is still listed. Each tick, while the stream is up, the supervisor compares `DeviceMonitor::callbacks()` with the count it last saw. It lives on the control thread; the callback's existing relaxed `fetch_add` is the only audio-thread part, nothing was added to it.

1. **Warning** after silence of `stall_limit` = max(`STALL_WARN_MIN`, `STALL_WARN_PERIODS` buffer periods): `warn` log and the `device_stalled` workspace warning. **Nothing stops.** `status.device.state` stays `ok`.
2. **Loss** after `stall_loss_limit` = max(`STALL_LOSS_MIN`, `STALL_LOSS_PERIODS` periods): `DeviceEvent::Lost` with reason `stalled`, handled exactly like an unplug; the loss replaces the warning.

Limits are sized from the stream's `block_frames` and `sample_rate` at open (unknown buffer: the floors). Big buffers scale: 4096 frames at 44.1 kHz warns at 1.86 s, lost at 4.64 s. 500 ms is ten times a large normal period; 3 s outlasts recoverable pauses seen on slow disks and USB wakes.

Rules (each one fixes a false stop):
- **Only watched silence counts.** A gap between two samples above max(`STALL_MAX_GAP`, warning limit / 8) restarts the window, and each stage needs `STALL_MIN_SAMPLES` samples inside it. A frozen process (both threads stopped, the control thread waking first with an unchanged count) is therefore never a stall.
- **Grace:** a stream gets one loss limit for its first callback after open (slow first IOProc, a Bluetooth route, an interface re-locking its clock), and again after a wake from sleep. No warning during grace.
- **Sleep detection:** on macOS the monotonic `Instant` stops during sleep, so it shows no gap. A second injected clock that counts sleep (`SleepClock`, `SystemTime::now`) moving on more than `SLEEP_SLACK` further than the monotonic one between two samples is a wake. An NTP step forward does the same, harmlessly.
- **Flapping:** a device calling back every 0.5-3 s is never silent for 3 s but plays almost nothing. (a) The warning clears only after callbacks ran one warning limit with no silence of that length; an `info` log says so. 
  (b) The warn log is written at most once per `STALL_LOG_EVERY` with the stalls since, and the clearing is logged only if the raise was. 
  (c) Over a sliding `STALL_WINDOW` of watched time (once `STALL_WINDOW_MIN` was watched, grace excluded), a stream that delivered half its audio or less is lost, reason `stalled (intermittent)`. Delivered audio is `DeviceMonitor::frames_delivered` at the stream's rate; silence is watched time minus that.
  So callback size, spacing and the reported buffer don't matter: a bursty stream delivering everything stays up, and one 600 ms stall is 6% of the window and recovers. **Never judge by ticks without a new callback:** the supervisor samples every 5 ms at most, so a healthy stream with a longer period looks still on most ticks, and doing so lost healthy large-buffer streams.
  An empty callback delivers nothing but holds off the two stages. With no known rate only the two stages apply. Time and frames from a gap that is no evidence (a freeze, a sleep) count for nothing.
  In tests, a watched clock move must be paid for with frames: `DeviceShow::advance` does this up to `STALL_MAX_GAP`; `advance_between_callbacks` moves the clock with no callback.
- **Only `Stream::Up` is watched** (not closing, down or opening). openLab never pauses a stream and a silent mixer still calls back, so "paused by us" reduces to "not up".
- **List vs watchdog:** a device that left the list is found by the list check first ("no longer listed"); the watchdog never words a loss that way.

## Device-loss policy (show safety)

Decided from measuring QLab 5; there is no grace period and no exact-sample resume. On `DeviceEvent::Lost` of one device, at once, whatever the outage length (`control/device_events.rs`):
- Patches on the device are reported `device_missing` (`Availability`): loaded cues on them unload, a start is refused as broken, idle cues show broken.
- Every active **audio** run with a voice in that device's mixer, or whose patch is mapped to that device, ends with `ActionFailed { device_lost }`: pre-waiting, playing (voice started or waiting for its probe), paused, fading, stopping and post-waiting alike, with or without a voice.
  Hard stop with no fade; voice and stream are released; its pre-wait, auto-continue and auto-follow are cancelled (core `action_failed`). A group hears a stop.
- **What it shows** (QLab 5, skill `qlab-behaviour` C-X1, Q390-Q396): a cue that was playing or fading shows `paused` (wire: `cue_state` `paused` with reason `device_lost`) with its time standing still; one counting its pre-wait or post-wait shows `paused` with that countdown frozen. Neither is a real pause: it owns no run, voice or deadline (core `Slot::Lost`), so nothing of it can sound or fire.
  A cue paused by the operator, one fading out (a stop with a fade, a panic) and an idle or loaded cue routed to the device show `broken` (`device_lost` for a run, a `device_missing` problem otherwise).
- Every voice that device's mixer holds is stopped too (paused ones, fading tails, orphans), dated on the first sample the mixer renders after the outage (`Voices::stop_date` on the primary; another device's commands are dated so while it is away), so nothing sounds when it returns.
- Those voices, and the doc's voices on that device whose end was handled since the last meter read, are never metered again (`Voices::unmeter`), and the device's output and patch bus peaks are emptied when it is back (`Engine::drop_held_meters`): what its mixer rendered before the loss lights no meter, while the device is away or when it returns.
- A voice of a failed cue that the engine no longer owns (the end of a sound in its post-wait, completed up to a block ahead) is stopped on whichever device it plays (`silence_orphans`): a cue moved to the lost device can still sound on the one it came from.
- An `error` log counts the stopped cues; a `device_lost` runtime notice and workspace warning go out.
- **Last device:** when no device plays any more (the only device, or the last of several), every active audio run fails as above and the loop then feeds `Input::DeviceLost`, which hard-stops every other active run (memo, Wait, control, Fade, group, and runs already broken but holding a pending continue): every pre-wait, post-wait, auto-continue and auto-follow set before the loss is cancelled. Loaded and settled broken cues keep their state.
  QLab-confirmed for waits with one interface: unplugged during a pre-wait or post-wait it starts nothing after it is plugged back, and the countdowns do not keep running.
- **Another device still plays:** only that device's cues stop; every other run, wait and cue carries on (user decision, skill `qlab-behaviour` Q255).
- **After the loss:** a playing cue's paused mark stays when the device returns; Resume or Resume All only clears it (idle, or broken while the device is still away); GO with the device back plays it from its start; Stop, Panic and Reset clear it. A waiting cue's mark clears by itself when the device returns (idle; it never fires). Broken cues are idle once it is back.
- **GO during an outage**, and any start on the device, is refused as broken (`cue_broken`); it is not queued for the return. A cue showing the paused mark keeps it. Cues with no output on the device run as usual (on the frozen clock when no device plays).
- **Stop/pause in an outage** find nothing on the device; the ones that reach an orphan voice are dated on the first sample after return, never before the voice's own start. **Panic while no device plays** is a hard stop (a second Panic when a panic fade was already under way, since a fade could only play after the outage).
- **Back:** the stream reopens, patches report `mapped`, cues broken by `device_lost`, waiting cues it left paused and idle cues routed to it become playable (idle), and the `device_lost` warning goes; a cue it stopped while playing keeps its paused mark (see skill `cue-engine`, broken cues).
- A stop sent to a voice while the mixer is silent must be dated on the cut sample, or the voice plays one more block. A flapping device just stops the cues on it each time; no voices or streams leak.

## Several devices (AUD-02)

- **Opening.** After a mapping or list change, `Control::open_outputs` opens each listed device a mapping names that is not open yet: a mixer of its own (`ControlConfig::output_mixer` with the device's rate and outputs; `None` turns this off, as for the offline driver) and a `Device` started from the primary's (`Device::start_another`, the layer's `another()`) on its own thread, without waiting.
  Until its first open answers it is down and its patches are `device_missing`; a failed open retries with the reopen backoff. At most `MAX_OUTPUTS`.
- **Each device's own rate and outputs** (`negotiate::choose_output`, pure): the primary's rate when the device can run at it, else the first of 48, 44.1, 96, 88.2, 192, 176.4, 32, 22.05, 16 kHz it offers, else the highest it offers that openLab resamples to (8 to 384 kHz);
  as many outputs as it has at that rate, at most 64.
  An `info` log names a rate other than the primary's and the output count. A device offering no usable configuration or rate is not opened (one `warn` per device, reason in `unusable`); its patches stay `device_missing`.
  A patch mapped beyond a device's outputs resolves `channels_missing` with that device's own count (`Patches::within`); the `outputs_dropped` warning names the device's real count, or says openLab uses only its first 64.
- **Rates.** What the core counts (lengths, positions, offsets, delays, elapsed time, fades) is in master-clock frames, the primary's rate. A voice's stream is made at its patch's device's rate (`DiskStreamer::prepare_at`, the planned table rescaled with `openlab_audio::rescale`), so the file is resampled once, on the disk thread, straight to that device's rate: nothing on the audio thread resamples.
  A prime or loaded stream made for another rate than the patch's device has at the start is made again. `Outputs::route` converts each command's fades and trims to the device's frames; dates and events go through the clock translation below, scaled by the two rates.
  A loop's length rounds to a whole frame at the device's rate, and the device repeats that every pass (up to 0.5 frame a pass: 227 to 833 ppm for short loops, far more than clock drift).
  So a voice record keeps the device's table and cuts, and every count the core sees (`ActionLength`, positions, the file position shown, a devamp's rest, a move's plan) is derived from that table through its `Scale` (`voices/mod.rs`), one rational rounding of the whole count: the end, an auto-follow, a devamp's leave and a move stay within a frame of the sound after any number of passes.
  A move also counts the old voice's drift and underruns (skill `engine-control` reference `patch-slots`), the drift as smoothed, which follows a steady drift 2 s late (1.6 frames at 100 ppm and 8 kHz).
- **Patch matrices** are kept at the widest mixer's stride; each output's mixer gets only the columns it has (`Patches::set_width`).
- **Patch warnings** (QLab 5 manual p. 191, `device_warnings`): a patch `device_missing` on a device the list lacks gets `patch_device_missing`; on a listed device that cannot be used (`Engine::unusable`: no usable rate, too many open, a listed device's first open that failed; cleared when its stream comes up or it leaves the list) `patch_device_unusable` with the reason, also the resolution's `reason`; nothing while a first open is under way.
- Every notice about one device (`device_lost` at the loss and in force, `device_unrecoverable`, `device_stalled`, `outputs_dropped`, `patch_device_missing`, `patch_device_unusable`, `patch_routing_beyond_device`) carries the device's name in `device` (`device_notice`), so a client lists one entry per device; the `message` stays the log's English.
- A patch set to no device gets `patch_no_device` while some cue plays on it (an Audio cue, or a Video cue not known to have nothing to hear: `CueEngine::cues_on_patch`); a spare patch gets none, so it is no pre-show problem. Each goes when its cause does (a cue added, moved or removed re-checks it).
- Every notice about one patch (the above, `patch_no_system_output`, a patch's `outputs_dropped`, `patch_routing_beyond_device`, `mixer_patches_exhausted`) names it in `patch` (`about_patch`); the pre-show check leaves out those of a patch no cue plays on, the `warnings` list keeps them.
- A device's own notices (`device_lost`, `device_stalled`, `device_unrecoverable`, its `outputs_dropped`) count in the pre-show check only while some cue's patch is on that device (through the system output too), the primary's loss, stall or failure whenever the workspace has any cue (a stall of it freezes the master clock for every timed cue until it is lost; its `outputs_dropped` touches no clock); a lost device only spare patches map to stays in the `warnings` list and passes the check.
- **Closing.** A device no patch of any open doc maps to any more is parked (`Device::park`) once no voice plays on it (a device that is away is parked at once: its voices were stopped at the loss, and their records are settled as ended, so none is kept and nothing of them sounds if it returns).
  A parked device's stream closes, and it is not watched, reopened or reported lost, so unplugging it raises nothing. A patch mapped to it again unparks it: it opens again at once (with the reopen backoff if that fails, without a `device_lost` warning meanwhile).
- **Routing.** Slot numbers are shared by every mixer; a slot's gains go to the mixer of its patch's device (`Patches::slot_output`). A voice plays in the mixer of its start's slot (`Outputs::route`); every later command of it goes there.
- A voice never moves between mixers: a cue whose patch moves to another device moves there by a new voice on that device's mixer that takes over in step, crossfading (`RelocateVoice`, skill `engine-control` reference `patch-slots`); the old device's voice ends there and the control thread frees it. Each device keeps its own clock: the two voices are dated in master time and each lands on its own device's sample for it.
- **Clock.** The primary's sample counter is the master clock while it plays. Another device's commands are dated `local_rendered + (at - master_rendered) × local rate / master rate` (the primary's while it is away: `local_rendered + (at - master_rendered)`) from the counters read when they are sent,
  never before what that device has rendered and never before an earlier command of the same voice; its events come back through the inverse. Starts on two devices land within one buffer of each other (the counters are read between callbacks), within a frame when the two run in step, and then drift with the devices' clocks (as QLab).
- **Drift.** Each tick (`follow_clock`) every other device's counter is read against the master's into its `Skew` (master frames, smoothed over `SKEW_SMOOTHING_S`, 2 s: a reading is off by up to a buffer of each).
  A voice there records the skew at its start, holds its drift through a pause and carries it to its resumed id; `Outputs::drift` is how far it has played past the master clock. Audio cues are not corrected (QLab lets each device keep its clock); a Video cue's picture follows it (skill `video`, Sound).
- **Primary away, another device playing:** the master clock runs on the primary's wall clock from the last sample it rendered; when it returns its counter is offset to go on from there. With no device playing it stands still. It never goes back (`Outputs::follow_clock`).
- **Buffer size change** (`Device::set_buffer`): a device that plays closes and opens again at once at the new size with its mixer as it was (voices and its counter carry on after the gap; `DeviceEvent::Reconfigured`, not an outage). The other devices are untouched.
  During the gap it does not play (`Device::is_playing`): its patches stay mapped, its commands are dated on its first sample back, and while it is the primary the master clock runs on the wall clock if another device plays (stands still if none).
  Cost, accepted: in that case the primary's voices wait out the gap while the clock runs on, so their cues' planned ends, and the auto-follows on them, come early by the length of the gap (milliseconds for a normal change).
  A reopen that fails, a close or reopen the device thread does not finish within `DEVICE_JOB_TIMEOUT` (with `device_unrecoverable`), or a mixer lost in it, is a loss (`DeviceEvent::Lost`): only its cues stop; as the sole or last device, as any sole-device loss.
- **Lookahead** is the largest buffer of the devices that play, in master frames (a block at another rate rounded up, `master_block`), so every deadline is committed before the callback that renders it on each.
- Mixer stage faults are each device's: its mixer's disabled units are its own (a rebuild clears only them), and with several devices their logs and warnings name the device.
- Engine load, xruns and underruns are summed or the worst over every device. `status.device` is the primary; `status.outputs` lists every output opened since start with its state (`Output::state`: up, opening, lost, reconfiguring, closed when parked, failed when unrecoverable), its own rate, channels and buffer (channels 0 and buffer none before its first open), and which is the primary.
- `list_devices` marks each listed device the engine has an output on with that state and the primary, matched by host id (two interfaces can share a name); `list_patch_mappings` gives each patch's current resolution (`Patches::resolution`).

## System output (QLab 5 manual p. 7)
- A patch mapping with `system` plays on the device the last list read marks `is_default` (`Engine::system_output`; none for the offline driver): `PatchMapping::device_for` gives the device name every user of a mapping goes by (resolution, which devices to open, a loss's cues, refits).
- The operator changing the system's output shows as a list change: patches resolve again, that device opens as another output if it is not open, and a cue playing on the patch moves there at once (`follow_devices`, skill `engine-control` reference `patch-slots`). The old device is parked once nothing plays on it. No default output: the patch is `device_missing` with device `system output` and a cue playing on it stops as on a lost device.
- The primary is unaffected: it stays the device opened at start. A patch naming a device never follows the default.

## Default patch mapping

QLab 5 maps a new workspace's audio patch to the current output device automatically; openLab does the same, per patch, in the engine's `sync_patches` (start, doc opened, AddPatch, device back):
- Any patch with **no mapping entry** in this machine's `settings.json` (keyed by workspace id and patch id) gets its outputs 1..N mapped to the primary device's outputs 1..N, N at most the device's outputs. An existing entry is never overwritten, including an explicit empty entry (`{"device": "", "channels": []}`, how a cleared mapping is stored so the default does not re-apply).
- **Too-small device: the outputs it has.** As QLab 5 routes cue output n to device output n only where it exists, a patch wider than the device is mapped on the device's outputs and its later cue outputs get no default route; it plays and its cues are not broken. Only a device with no outputs leaves it `Unmapped` (one `warn` per patch and device).
- Primary not playing (none, or an outage): nothing is written; the next sync with it maps. Offline: the first configured device.
- **A new workspace** has exactly one patch, "Patch 1" (`Workspace::new_empty`), however many devices are listed (Q640, Q781): the default mapping above puts it on the primary device. A show opened on a machine without the other devices has no patch it never asked for.
- The settings write goes to the settings writer thread, never the control thread; a failed write is a `warn`. An `info` line says `mapped patch 'Patch 1' to <device> outputs 1-2`.
- No per-output delay exists (QLab 5 has none); a patch output's level, mute and polarity are its matrix gains.
- Setting or clearing a mapping by request (`SetPatchMapping`) is skill `engine-control`.

## Tests

- Device-layer tests use no hardware (`tests/device.rs`, unit tests in the three modules): negotiation including 64 and 128 channels and an empty list, host-driven output equal to the offline driver sample for sample in every format, handoff and loss with a 4-channel reopen, errors counted once, no allocation on any callback path or the handoff drop.
- Engine policy tests (`tests/control/devices.rs`) use a stand-in layer whose streams hold their mixers in `OutputHost`s rendered block by block, every open device in step (a device at another rate renders as long in time: `listed_at`), on the real device threads, with a test-driven wall clock. The driver waits for the device threads' answers on the inbox, never sleeps; `DeviceShow::play` moves the wall clock with the audio (the master clock while the primary is away).
- A real unplug cannot be automated here; check it by hand. `#[ignore]`d `hardware_smoke_default_output` plays 0.5 s of silence on the default device: `cargo test -p openlab-audio --test device -- --ignored`. On a Linux VM with no sound card, `ALSA_CONFIG_PATH` pointing at a file with `pcm.!default { type null }` runs it on ALSA's null device.
- The app's tests run on the virtual device: read `references/virtual-device.md`.

## References

- `references/virtual-device.md`: read when writing or debugging an app/UI test that needs an engine without hardware, or changing `control/virtual_device.rs`.
