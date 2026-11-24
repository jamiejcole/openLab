# Orchestrator state

Policy health (update at milestone end): 2026-11-11 open `low` 0, `severity:medium` 1 (after the M5 mediums PR); 2026-11-07 open `low` 13; baseline 2026-10-28 was 94. Mediums fixed in the last reviews: not tracked; claims the repo did not back: 1 (DOC-02 ticked with no OSC reference in `docs/`).

Rules learned:
- Delete a worktree and its `engine/target` once its branch is pushed (disks fill).
- Delete stale branches instead of pushing them: an old workflow file runs old CI.
- A red `main` is fixed through a PR before new work.
- Never reuse another agent's branch name in a scratch worktree.
- A mac test run from a new worktree builds a new UI test runner, and macOS asks again for accessibility access; until it is answered the UI smoke tests fail. Reuse one worktree for mac runs where possible.
- Test several small branches in one integration worktree (one build), then land them as one PR when the user asks for combined PRs.
- A test that compares against the list's visible width must force always-shown scroll bars (`scrollerStyle = .legacy`): the CI Mac shows them, a dev Mac may not.
- An agent short of disk must never link its build/ or engine/target into another worktree (its builds overwrote another branch's binaries, 2026-11-24): it deletes its own build output or stops and reports. Say so in every brief that builds.
- A test engine is shared by its whole test class: a test that plugs, unplugs or maps devices runs on its own `TestEngine.start()`.
- A CI app shard that run-mac-tests.sh ends with exit 75 (its sample shows WindowServer stuck in the GPU driver) is the hosted runner, so the cause is known: note it on the PR and re-run that job once; a second stall on the same PR is investigated.
