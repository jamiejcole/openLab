# Decode details: positions, priming, seeking, header checks, wide PCM

## Positions and continuity
- Position 0 is the first playable frame. The decoder applies every packet's encoder delay and padding itself, the same way for every codec: an MP3 with LAME gapless info has its original length; Vorbis follows its granule positions.
- The stream ends at the container's end trim (the packet it trims padding off), even if damage left more data after it.
- A packet never counts for more than its container duration, whether or not it decodes (a failed packet is silence of that duration), so the timeline does not depend on decoder state. This also drops AAC padding past the MP4's length.
- When the track's time base is a whole fraction of a frame (every supported format), each packet's position comes from its timestamp: a gap before it is filled with silence (`Missing`), an overlap is dropped (`Repeated`).
- Otherwise positions are counted and a seek decodes forward from the start (an MP4 whose timescale is not a multiple of the rate, e.g. 90000 at 44.1 kHz; also any seek the container refuses). Every loop seam then costs time proportional to its position in the file, so preload such files if they loop. A seek into a gap gives the same silence as reading through.

## AAC priming in MP4
symphonia 0.6.1 ignores the edit list and the iTunes gapless tag, so `mp4_priming.rs` reads them before symphonia parses the file (only AAC tracks use it).
- It is a bounded box walk like `mp4_check.rs` (limits as constants in the file; no allocation from file-supplied lengths).
- Priming is the audio track's single `elst` entry at normal speed (`media_time`, scaled from the track's `mdhd` timescale to its rate), else the second field of the `iTunSMPB` tag. The edit list wins; the tag is also the fallback for an edit list that is not understood (several entries, negative media time, rate other than 1, no timescale). An edit list of zero means no priming.
- Position 0 is the frame after the priming. It is part of the decoder's `origin`: frames before it have negative positions and every packet drops them, whether met by probe, preload, streaming or after a seek (a seek to frame F decodes the stream's frame F + priming and lands on real frame F).
- Probe length and duration exclude the priming; the container's declared length is compared net of it.
- A value that is absurd (32768 frames or more, or not less than the container's length), negative or overflowing, a malformed tag, or an unreadable edit list with no tag is ignored with a `PrimingIgnored` warning and the file plays as decoded.
- If the real stream is shorter than the priming, nothing is left: `Corrupt("no audio frames")`.
- ALAC, MP3 and Vorbis are unaffected (their delay is handled by the codec or container reader).

## Seeking
**Sample-accurate seeks.** A seek starts a fresh container reader and a fresh decoder (symphonia 0.6.1's FLAC reader returns stale frames after some seeks; its AAC `reset` keeps state), seeks to a pre-roll before the target (8192 frames for lossy codecs, none for lossless), drops the first packet after a mid-file lossy seek (it lacks its predecessor's overlap or bit reservoir), and discards frames up to the target by position.
- A seek that lands after its target is retried with twice the pre-roll, then from the start. One the container refuses decodes forward from the start.
- Afterwards every format yields exactly the frames a decode from the start yields, except AAC with noise substitution (ffmpeg's encoder uses it): those bands are pseudo-random and differ by under 0.003 after a mid-file seek.

**Damaged files.** After damage, symphonia's readers can place frames differently after a seek than when reading through, so a damaged file's seeks are checked against the probe's own decode.
- The probe's full decode records a seek index: a hash of the 256 timeline frames at every `INDEX_SPACING` (8192) frame, plus whether the window is flat (digital silence or nearly constant, under about -80 dBFS peak to peak). A flat window verifies at any alignment, so seeks and the probe skip flat checkpoints and use the nearest non-flat one.
- `Seeking::Indexed`: seek to the nearest checkpoint before the target, check that the 256 frames there decode exactly as the probe decoded them, decode forward at most one spacing. If that checkpoint does not match, try the two before it, then decode from the start. The probe verifies the checkpoint after each piece of damage and the last one before choosing this.
- `Seeking::FromStart`: those checks failed. The probe adds `SlowSeeks` and `Probed::preload_advised()` is true: preload if it fits the budget.
- `Seeking::Direct`: undamaged files (and PCM files that are only truncated).
- Per format (tested bit-exact against preload from offsets across the file and around damage): FLAC and Vorbis get `Indexed`; MP3 gets `Indexed`, or `FromStart` when the reader misplaces frames after damage; AAC always gets `FromStart`, because noise substitution never decodes identically after a seek.

## Headers are checked before symphonia
Fuzz-found: symphonia's `u16` frame-size arithmetic overflows on bad headers (a panic with overflow checks on, a wrong frame size without). Pre-checks return `Corrupt("header: ...")` before it parses:
- WAV `fmt ` or AIFF `COMM` with 0 or more than 64 channels, more than 64 bits per sample, or a `WAVE_FORMAT_EXTENSIBLE` channel mask symphonia would shift out of range.
- MP4 `stts` tables whose sample counts or durations overflow (`mp4_check.rs`).
- The check covers the container at byte 0 and every `RIFF....WAVE` / `FORM....AIFF|AIFC` marker in the first 1 MiB that the probe would scan on to (at most 64), and follows each container's chunks as symphonia's readers do (`riff_check.rs`).
- That is not the declared sizes alone: their parsers leave the reader off the declared sizes (a `fmt ` chunk is not skipped to its end, `MARK`/`COMT`/`ID3 ` entries can run past theirs, an odd `LIST` moves two padding bytes). So every `fmt ` symphonia parses before `data`, and the first `COMM`, are the ones checked. More than 1024 chunks in one container is `Corrupt`.
- Other numeric header fields (rates, frame counts, chunk sizes, offsets) symphonia checks or computes in `u64`; the sample rate is then range-checked.
- **On a symphonia upgrade, re-check `riff_check.rs` against its RIFF readers.**
- The decode fuzz target covers the reader through `probe_media`, `preload` and `open_source`. Add a seed when you fix a header bug.

## Many-channel PCM (`pcm.rs`)
symphonia reads WAV only for layouts it knows (about 18 to 26 channels), so a 64-channel file would not play although voices carry 64.
- Uncompressed WAV and AIFF with 9 to 64 channels (`NATIVE_MAX_CHANNELS` = 8 is the most symphonia is trusted with) are read by the in-house `PcmSource` behind the same `FrameSource` interface. `probe`, `preload`, disk streams and the waveform all go through `open_media`, which picks the reader from the header. Nothing on the audio thread changes.
- Integer samples scale by `2^-(bits-1)`; floats are sanitised like every decode (non-finite to 0, clamp +-16).
- **The WAV channel mask is ignored.** openLab routes by channel index, so a mask naming more, fewer or different speakers than the file has channels plays the same.
- Header checks: the `fmt `/`COMM` chunk must be complete; block align must equal channels x sample bytes; an extensible header needs the standard or ambisonic PCM/float sub-format GUID and valid bits within its container; rate in range; at most 64 chunks walked, each size checked against the file length before skipping. Failures are `Corrupt`, or `Unsupported` for a sample size, format tag or AIFF-C compression it does not read.
- **Length** is the frames the file holds, never more than the header declares. A data chunk or AIFF frame count above the file gives a `Truncated` warning (`at` = frames present) and plays to the real end; a WAV data size of `0xFFFFFFFF` means "to the end of the file"; no whole frame is `Corrupt("no audio frames")`.
- A seek is a byte offset (`data_start + frame x frame_bytes`): exact, one `seek` call. A file that shrinks after its probe gives `Changed`.
- Reads are at most 4096 frames. No length from the file sizes an allocation, and a preload checks its budget from the frame count before reserving.
- Compressed formats keep symphonia's channel limits. An AIFF-C with other compression and more than 8 channels is `Unsupported`; a WAV with a tag other than PCM, float or extensible (A-law, mu-law, ADPCM) stays on symphonia's path.
