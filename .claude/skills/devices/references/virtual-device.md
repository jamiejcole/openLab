# Virtual device (test-support builds)

`engine/crates/engine/src/control/virtual_device.rs`, compiled only with the engine's `test-support` cargo feature: the real engine with no audio hardware, for the app's tests. `openlab-engine --virtual-device stepped|realtime` (usually with `--app-support DIR` and `--watch-stdin`) plays on **openLab Virtual Output** (id `openlab:virtual`, 2 channels, 48 kHz, 256-frame blocks) instead of cpal's devices.
- A second device, **openLab Virtual Output 2** (id `openlab:virtual-2`, the same format), is listed only once `restore_device` names it, so a test can map a patch to it and play on two devices (AUD-02). Without it the list is as before, so tests of one device see one.
- The first is the system's output at start (`is_default`); `set_default_device` moves the mark. An open with no device id opens the marked one.

A build without the feature has no such option: a usage error, exit 2. Test-only code never compiles into Release; `mac/scripts/verify-bundle.sh` checks the bundled release engine refuses the option and holds no virtual device.

## Same policy as a real device

- `VirtualDevice` is a `DeviceLayer` like `CpalDevices`. The stream opens, closes, is lost and reopens on the device thread through `devices::Device`, so list reads, loss, backoff and both watchdogs behave as for a real interface (skill `devices`).
- Each open stream has a render thread (`openlab-virtual-audio`) owning the mixer's `OutputHost`, calling `OutputHost::render` into a buffer allocated before its loop. The loop takes no lock, allocates nothing, and learns what to do from atomics. Closing joins the thread and the mixer returns through the `HostLink`, as from cpal.
- Each device has its own atomics (`Port`: listed, lost, rendering, frames granted and done, its render thread); the mode, device time and the stepped requests' state are shared. The second device opens through the policy's own route for another output (`Device::start_another`, the layer's `another()`).

## Modes

- **Stepped:** device time moves only on the test-only `advance { frames }` request (wire shape in `docs/protocol.md`, "Test support"), handled on the control thread.
  - First it waits for background work (`settle`: probes, preloads, device jobs, document reads and saves, network sends, waveform reads, and the stage host's opens of pictures whose starts wait for them; at most 30 s, then `internal`).
  - Then per block of up to 256 frames: wait for the disk streams the block reads (`Control::wait_streams`, shared with the offline driver), have every open stream's render thread render it, in step, and wait for them, move device time, tick, settle again.
  - Device time is the frames rendered (while the device is down, the frames that would have been). The device policy's `WallClock` and `SleepClock`, the engine's `EpochClock` (from 2026-01-01 00:00:00 UTC), and so the status interval, autosave pacing and log times all follow it. The random seed is fixed; `status.cpu_seconds` and `engine_load` are absent.
  - Workers' answers arriving between requests (`Inbox::Probe`, `Device`, `Net`, `Watch`) are **held** and taken in at the next settle, which ticks only once no answer is outstanding (or after 50 ms without one), so what they change goes out at the same point of a script whatever their timing. Document answers and waveforms are not held (they answer a request).
  - The runner's 5 ms wakes still tick between requests; with device time frozen and answers held they change nothing. The probe and document watchdogs keep the system clock (they guard a hung disk).
  - Determinism: the same requests give the same broadcasts and results on each connection.
- **Realtime:** the render thread renders a block every block period on the system clock into a discarded buffer (UI tests). `advance` is refused (`unsupported`).

## Faults

- `lose_device { device? }` (the id; absent: the first): the device leaves the list and its open stream reports `HostError::Lost` once and never calls back; the policy stops the cues on it, closes the stream, and reopens fail (`not_found`) while it is gone. The other device plays on. Stepped, the request returns once that is done.
- `restore_device { device? }`: the device is listed (again); the next list read opens it at once.
- `set_default_device { device }`: the list marks that device as the system's output from the next list read. An unknown id is `not_found`.

## Differences from a show engine (on purpose)

- OSC listeners stay off (test engines run side by side and would race for port 53000).
- The engine never keeps the computer awake (no power assertion).

## Tests

Unit tests in the module: exact frames rendered and every clock following them, a second `advance` refused, shutdown during one, held answers taken in at the next `advance`, loss and return, realtime rendering, refusals without a virtual device, the second device plugged in and the default moved, streams on both rendering in step and each lost alone.
- `tests/net/session.rs` runs the real binary on its stepped device over its real socket: a three-cue show with GO, progress and panic giving the same messages on two runs; lose and restore over the wire; a patch on the second device heard there and not on the first; realtime refusing `advance`.
- App side: `TestEngine.deviceIDs` / `deviceNames`; the scenario step `device` takes `output` 1 or 2.
