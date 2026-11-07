# Writer thread, coalescing, stalls and shutdown (`persist/writer.rs`)

Serialising plus the verify-reload costs up to about a second for a large show, so saves run on the writer thread. The control thread clones the `Workspace` and calls `SaveWriter::save` / `SaveWriter::autosave`, tagged with the workspace's edit revision, and gets a channel with the result.

## Bounded queue by coalescing
The disk may be slow or hung while autosave keeps firing, so the queue is bounded:
- One pending manual save per target file. A newer one replaces it; the old answer is `Superseded`.
- One pending autosave per recovery target (workspace id plus show path, the pair that names the recovery file). A newer one replaces only its own target's; the old answer is `Ok(None)`. Several open workspaces never displace each other's autosaves.
- At most `MAX_PENDING_TARGETS` targets wait in the normal queue. A new target beyond that moves the oldest waiting autosave to a write-through queue that runs first, so the cap itself never costs an autosave. That queue still coalesces per target and is capped at `MAX_FORCED_TARGETS`, because during a long disk hang closed workspaces and Save As paths add targets beyond the open ones (at most 8 docs).
  When full, the oldest forced autosave is shed (`Ok(None)`, counted by `shed_autosaves()`). A later autosave of a still-open workspace replaces it anyway.
- Manual saves run before pending autosaves. An autosave whose revision is not newer than the last durably saved one of its workspace is dropped (`Ok(None)`), so saved state never gets a recovery file.
- `clear_recovery(workspace, show_path)` runs after any job in flight that writes that recovery file, removes that target's file, and cancels only that target's pending autosave.
- Dropping the writer finishes running jobs and pending manual saves and **discards pending autosaves**.

## Two threads never write the same file
A job waits while another thread runs one that writes the same show file or the same recovery file (matched by the name's stem; a save also removes two recovery files when done). Why: a stale write that returns late must not land over a newer one.

## A save that stalls
The writer thread can be stuck on a volume that stopped answering. Each tick the loop runs `SaveWriter::check_stalled` on `ControlConfig::stall_clock` (monotonic, so a wall-clock step neither fails a save nor postpones the watchdog).
- A save, autosave or recovery-file removal first seen running `ControlConfig::doc_timeout` (20 s) ago is answered `PersistError::Stalled`: a save's requester gets `io`, an autosave logs its usual `autosave failed` warning, an `error` log names the file.
- Its thread is abandoned (it leaves once the write returns, if ever) and a fresh thread takes over the queue, at most `MAX_WRITERS` threads in all. The cap is every open show's show file plus its recovery file stuck (2 x 8 docs = 16) with one thread left to serve the rest; a compile-time assertion in `control/mod.rs` holds this. With every thread stuck, saves and autosaves answer `io` at once until one returns.
- While a stalled write is out, saves and autosaves of the files it holds are refused at once (`io`) and recovery-file removals wait for it. Everything else saves as usual. Once it returns, its file saves again. Its late answer is not reported (the request was already answered).
- **A stalled save holds only its show file.** Given up on, it no longer removes the recovery files when it returns (an autosave since may hold newer edits). So the same workspace can still be saved elsewhere (Save As) and autosaved, including its final recovery write at shutdown.

## Shutdown
- `Control::shutdown` (after the hard stop) hands every doc with unsaved changes, whose last edit no autosave holds yet, to the writer as an autosave. It waits for those and for an autosave of the last edit already under way, in all at most `ControlConfig::shutdown_wait` (`SHUTDOWN_WAIT`, wall clock: shutdown is the one place the loop waits on it).
- A write that fails (an error, not a timeout or a missing writer: a transient disk error) is handed to the writer once more while the wait has time left, with a `warn` log, since the previous autosave lacks the last edits. A second failure is final.
- It returns `FinalWrites { written, failed, unfinished }`. Each failure and each write still running at the end is an `error` log naming the workspace; a clean run is one `info`.
- **This must come before the docs are dropped**: dropping `SaveWriter` discards queued autosaves. A caller that needs a final autosave must wait on its reply.
- When every writer thread is busy, `final_recovery_writes` adds a spare (`SaveWriter::add_spare`), so one show's slow write does not hold the others' final writes.
- The drop of the writer itself waits at most `persist::DROP_WAIT` (5 s) for running jobs and pending manual saves, then leaves the threads to finish on their own, so a hung volume cannot keep the process alive.
- Why a final write exists: a quit or `SIGTERM` loses nothing an autosave interval would have. The next launch's `open` offers it and `scan_recovery` lists it.
