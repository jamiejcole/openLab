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
| objc2, objc2-foundation, objc2-core-foundation, dispatch2 | Objective-C runtime, Foundation, CoreFoundation and the main queue for the stage host (`crates/video`, macOS only); already in the tree through cpal | MIT / Zlib OR Apache-2.0 OR MIT |
| libc | `_exit` for the stage host ending at once from any thread when the engine is gone (`crates/video`, macOS only); already in the tree through cpal and tokio | MIT/Apache-2.0 |
| objc2-app-kit | stage host output windows, screens, accessory activation policy (`crates/video`, macOS only) | Zlib OR Apache-2.0 OR MIT |
| objc2-core-graphics | display list, display UUIDs and pixel sizes, display reconfiguration callback (`crates/video`, macOS only) | Zlib OR Apache-2.0 OR MIT |
| objc2-metal | stage drawing (`crates/video`, macOS only) | Zlib OR Apache-2.0 OR MIT |
| objc2-quartz-core | `CAMetalLayer` of each output window (`crates/video`, macOS only) | Zlib OR Apache-2.0 OR MIT |
| cargo-fuzz's LLVM harness binding crate (named in `engine/fuzz/Cargo.toml`; fuzz crate only) | fuzz harness; not in shipped binaries | MIT/Apache-2.0 (the LLVM library: Apache-2.0 WITH LLVM-exception) |

Video crates approved by the user (2026-11-13) and added with the code that uses them: objc2-av-foundation, objc2-core-media, objc2-core-video (decoding, Video cues), objc2-image-io (still images), snap (HAP). Each is used with default features off and only the headers it needs; the stage host is the only crate that links them.

## Hand-declared system calls
Calls std lacks are declared next to their one use, each with a `// SAFETY:` comment and a test, not pulled in through a crate:
- `getrusage` / `GetProcessTimes` (`diag::cpu`)
- `pthread_sigmask` / `sigwait` (`platform::signals`)
- ColorSync `CGDisplayCreateUUIDFromDisplayID` (`openlab-video`, `platform::mac::displays`)
- IOKit `IOPMAssertionCreateWithName` / `IOPMAssertionRelease` and CoreFoundation `CFStringCreateWithBytes` / `CFRelease` (`platform::power`; macOS frameworks linked by name)

## No `tracing`
The show log's records are structured protocol values (`LogEntry`) that clients read with `log_tail`. A small writer thread in `diag/` (bounded channel, size rotation, drop-and-count) replaces the crate and its tree. Skill `engine-lifecycle`.
