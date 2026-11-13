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
- **The stream never follows the system default**, and the engine never switches device while running.
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
- `DeviceMonitor::take_engine_load()` swaps it to 0 and returns the fraction (1.0 = the callback took its whole block), or `None` when no callback ran. Single consumer: the status reporter.
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
- A gap right after a host-reported xrun is not counted again (a report arriving only after the next callback can still double count). A clock stepping back re-baselines without counting; callbacks without timestamps are not checked.
- **Latency:** `OutputStream::latency()`: `buffer_frames` (host buffer, else last callback size, else the requested size) and `device_seconds` (playback minus callback time from the last callback's timestamps, when usable; CoreAudio includes device latency and safety offset).

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
  Hard stop with no fade; the cue is `broken` with reason `device_lost`; voice and stream are released; its pre-wait, auto-continue and auto-follow are cancelled (core `action_failed`). A group follows its children's failures.
- Every voice that device's mixer holds is stopped too (paused ones, fading tails, orphans), dated on the first sample the mixer renders after the outage (`Voices::stop_date` on the primary; another device's commands are dated so while it is away), so nothing sounds when it returns.
- An `error` log counts the stopped cues; a `device_lost` runtime notice and workspace warning go out.
- **Last device:** when no device plays any more (the only device, or the last of several), every active audio run fails as above and the loop then feeds `Input::DeviceLost`, which hard-stops every other active run (memo, Wait, control, Fade, group, and runs already broken but holding a pending continue): every pre-wait, post-wait, auto-continue and auto-follow set before the loss is cancelled. Loaded and settled broken cues keep their state.
  QLab-confirmed for waits with one interface: unplugged during a pre-wait or post-wait it starts nothing after it is plugged back, and the countdowns do not keep running.
- **Another device still plays:** only that device's cues stop; every other run, wait and cue carries on (user decision, skill `qlab-behaviour` Q255).
- **Shown state:** broken (`device_lost`) while the device is away (also any cue routed to it), idle once it is back. QLab shows a pause mark that only Resume All clears; openLab has none, so Resume All does nothing to the cue and GO plays it from its start (progress was reset).
- **GO during an outage**, and any start on the device, is refused as broken (`cue_broken`); it is not queued for the return. Cues with no output on the device run as usual (on the frozen clock when no device plays).
- **Stop/pause in an outage** find nothing on the device; the ones that reach an orphan voice are dated on the first sample after return, never before the voice's own start. **Panic while no device plays** is a hard stop (a second Panic when a panic fade was already under way, since a fade could only play after the outage).
- **Back:** the stream reopens, patches report `mapped`, cues broken by `device_lost` and idle cues routed to it become playable, and the `device_lost` warning goes (see skill `cue-engine`, broken cues).
- A stop sent to a voice while the mixer is silent must be dated on the cut sample, or the voice plays one more block. A flapping device just stops the cues on it each time; no voices or streams leak.

## Several devices (AUD-02)

- **Opening.** After a mapping or list change, `Control::open_outputs` opens each listed device a mapping names that is not open yet: a mixer like the primary's (`ControlConfig::output_mixer`; `None` turns this off, as for the offline driver) and a `Device` started from the primary's (`Device::start_another`, the layer's `another()`) on its own thread, without waiting.
  Until its first open answers it is down and its patches are `device_missing`; a failed open retries with the reopen backoff. At most `MAX_OUTPUTS`.
- Every device runs at the primary's rate and output count. Channels a device has beyond the primary's output count cannot be used yet: a patch mapped to them resolves `channels_missing` (available: the usable count) and its `outputs_dropped` warning says openLab uses only the first outputs, not that the device lacks them. One that cannot do the rate is not opened (one `warn` per device); its patches stay `device_missing`.
- **Closing.** A device no patch of any open doc maps to any more is parked (`Device::park`) once no voice plays on it: its stream closes, and it is not watched, reopened or reported lost, so unplugging it raises nothing. A patch mapped to it again unparks it: it opens again at once (with the reopen backoff if that fails, without a `device_lost` warning meanwhile).
- **Routing.** Slot numbers are shared by every mixer; a slot's gains go to the mixer of its patch's device (`Patches::slot_output`). A voice plays in the mixer of its start's slot (`Outputs::route`); every later command of it goes there. A voice never moves between mixers: a cue whose patch moves to another device plays on where it started.
- **Clock.** The primary's sample counter is the master clock while it plays. Another device's commands, and the primary's while it is away, are dated `local_rendered + (at - master_rendered)` from the counters read when they are sent, never before what that device has rendered and never before an earlier command of the same voice; its events come back through the inverse. Starts on two devices land within one buffer of each other and then drift (as QLab).
- **Primary away, another device playing:** the master clock runs on the primary's wall clock from the last sample it rendered; when it returns its counter is offset to go on from there. With no device playing it stands still. It never goes back (`Outputs::follow_clock`).
- **Buffer size change** (`Device::set_buffer`): a device that plays closes and opens again at once at the new size with its mixer as it was (voices and its counter carry on after the gap; `DeviceEvent::Reconfigured`, not an outage). The other devices are untouched.
  During the gap it does not play (`Device::is_playing`): its patches stay mapped, its commands are dated on its first sample back, and while it is the primary the master clock runs on the wall clock if another device plays (stands still if none).
  A reopen that fails, a close or reopen the device thread does not finish within `DEVICE_JOB_TIMEOUT` (with `device_unrecoverable`), or a mixer lost in it, is a loss (`DeviceEvent::Lost`): only its cues stop; as the sole or last device, as any sole-device loss.
- **Lookahead** is the largest buffer of the devices that play, so every deadline is committed before the callback that renders it on each.
- Engine load, xruns and underruns are summed or the worst over every device; `status.device` and `list_devices` describe the primary.

## Default patch mapping

QLab 5 maps a new workspace's audio patch to the current output device automatically; openLab does the same, per patch, in the engine's `sync_patches` (start, doc opened, AddPatch, device back):
- Any patch with **no mapping entry** in this machine's `settings.json` (keyed by workspace id and patch id) gets its N outputs mapped to the primary device's outputs 1..N. An existing entry is never overwritten, including an explicit empty entry (`{"device": "", "channels": []}`, how a cleared mapping is stored so the default does not re-apply).
- **Too-small device: no partial mapping.** A partial one would silence and break cues and block later self-healing. The patch stays `Unmapped` with one `warn` per patch and device (`device X has M outputs, patch 'P' needs N: left unmapped`); a bigger or different device is tried at the next sync.
- Primary not playing (none, or an outage): nothing is written; the next sync with it maps. Offline: the first configured device.
- The settings write goes to the settings writer thread, never the control thread; a failed write is a `warn`. An `info` line says `mapped patch 'Main Outputs' to <device> outputs 1-2`.
- No per-output delay exists (QLab 5 has none); a patch output's level, mute and polarity are its matrix gains.
- Setting or clearing a mapping by request (`SetPatchMapping`) is skill `engine-control`.

## Tests

- Device-layer tests use no hardware (`tests/device.rs`, unit tests in the three modules): negotiation including 64 and 128 channels and an empty list, host-driven output equal to the offline driver sample for sample in every format, handoff and loss with a 4-channel reopen, errors counted once, no allocation on any callback path or the handoff drop.
- Engine policy tests (`tests/control/devices.rs`) use a stand-in layer whose streams hold their mixers in `OutputHost`s rendered block by block, every open device in step, on the real device threads, with a test-driven wall clock. The driver waits for the device threads' answers on the inbox, never sleeps; `DeviceShow::play` moves the wall clock with the audio (the master clock while the primary is away).
- A real unplug cannot be automated here; check it by hand. `#[ignore]`d `hardware_smoke_default_output` plays 0.5 s of silence on the default device: `cargo test -p openlab-audio --test device -- --ignored`. On a Linux VM with no sound card, `ALSA_CONFIG_PATH` pointing at a file with `pcm.!default { type null }` runs it on ALSA's null device.
- The app's tests run on the virtual device: read `references/virtual-device.md`.

## References

- `references/virtual-device.md`: read when writing or debugging an app/UI test that needs an engine without hardware, or changing `control/virtual_device.rs`.
