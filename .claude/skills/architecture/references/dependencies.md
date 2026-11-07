# Dependency register

All dependencies must be GPLv3-compatible (`cargo deny` enforces licences). Every dependency needs a row here. Prefer std or an existing crate.

| Crate | Purpose | Licence |
|---|---|---|
| cpal | audio device I/O (CoreAudio, WASAPI) | Apache-2.0 |
| symphonia | decoding WAV/AIFF/FLAC/MP3/AAC/OGG | MPL-2.0 |
| rubato | resampling | MIT |
| rtrb | lock-free SPSC queues | MIT/Apache-2.0 |
| crossbeam-channel | control-thread inbox | MIT/Apache-2.0 |
| assert_no_alloc | RT allocation guard in debug/test (a normal dependency of openlab-audio for the callback guard and of openlab-engine for the debug global allocator) | BSD-1-Clause |
| rosc | OSC encode/decode | MIT/Apache-2.0 |
| tokio, tokio-tungstenite | async networking, WebSocket | MIT |
| socket2 | OSC UDP socket buffer sizes (macOS refuses a reply larger than its default send buffer) | MIT/Apache-2.0 |
| futures-util (`sink` only) | `StreamExt`/`SinkExt` on the WebSocket stream | MIT/Apache-2.0 |
| getrandom | OS CSPRNG for the session token (std has none) | MIT/Apache-2.0 |
| serde, serde_json | serialisation | MIT/Apache-2.0 |
| uuid | cue/workspace ids | MIT/Apache-2.0 |
| thiserror | error types | MIT/Apache-2.0 |
| dirs | per-platform app-support paths | MIT/Apache-2.0 |
| unicode-normalization | NFC normalisation of media paths (core; pure) | MIT/Apache-2.0 |
| tempfile (dev) | temp dirs in persistence tests | MIT/Apache-2.0 |
| proptest (dev) | property tests | MIT/Apache-2.0 |
| cargo-fuzz's LLVM harness binding crate (named in `engine/fuzz/Cargo.toml`; fuzz crate only) | fuzz harness; not in shipped binaries | MIT/Apache-2.0 (the LLVM library: Apache-2.0 WITH LLVM-exception) |

## Hand-declared system calls
Calls std lacks are declared next to their one use, each with a `// SAFETY:` comment and a test, not pulled in through a crate:
- `getrusage` / `GetProcessTimes` (`diag::cpu`)
- `pthread_sigmask` / `sigwait` (`platform::signals`)
- IOKit `IOPMAssertionCreateWithName` / `IOPMAssertionRelease` and CoreFoundation `CFStringCreateWithBytes` / `CFRelease` (`platform::power`; macOS frameworks linked by name)

## No `tracing`
The show log's records are structured protocol values (`LogEntry`) that clients read with `log_tail`. A small writer thread in `diag/` (bounded channel, size rotation, drop-and-count) replaces the crate and its tree. Skill `engine-lifecycle`.
