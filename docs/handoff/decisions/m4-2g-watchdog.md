# M4.2g device watchdog: stall limit and where it lives

Decision (orchestrator, 2026-10-27).

- **What:** a running stream whose callback counter does not advance for `STALL_LIMIT = max(500 ms, 20 x buffer period)` of injected wall-clock time, with its device still listed and no host error, is treated as a device loss with reason `stalled` (M4.2g policy: cues on it stop at once, no automatic resume, close then the existing reopen path, cues un-break on return).
- **Why a loss and not a warning:** a hung stream plays nothing, so the show has already stopped; reporting it as a loss gives the operator the same visible state, the same cue stops, and a reopen attempt that may fix it. Waiting longer only prolongs silence.
- **Why 500 ms / 20 periods:** a healthy callback comes every 1-50 ms (buffers of 64 to 2048 frames); 500 ms is ten times a large normal period, long enough that a scheduling hiccup or a slow USB wake never trips it, short enough to react within a cue's attack. 20 periods covers the unusual big buffers (4096 frames at 44.1 kHz is 1.9 s) without making the floor too loose for small ones.
- **Where:** the device supervisor on the control thread (`devices::Device::poll`, every tick), not the audio thread. The audio callback already increments `DeviceShared::callbacks` (relaxed); nothing is added to it, so real-time rules are untouched.
- **Clock:** the injected `WallClock`, as the rest of the device policy, so tests are deterministic.
- **No false positives:** only `Stream::Up` is watched (closing, down, opening are not). openLab never pauses a stream and a silent mixer still calls back, so "paused by us" reduces to "not up". A reopened stream arms at its open (the first callback has one limit: startup grace). Two supervisor samples more than 5 s apart (Mac sleep/wake, a suspended process) are no evidence: the watchdog re-arms instead of declaring a stall.
