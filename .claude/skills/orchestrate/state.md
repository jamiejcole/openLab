# Orchestrator state

Policy health (update at milestone end): 2026-11-07 open `low` 13; baseline 2026-10-28 was 94. Mediums fixed in the last reviews: not tracked; claims the repo did not back: not tracked.

Rules learned:
- Delete a worktree and its `engine/target` once its branch is pushed (disks fill).
- Delete stale branches instead of pushing them: an old workflow file runs old CI.
- A red `main` is fixed through a PR before new work.
- Never reuse another agent's branch name in a scratch worktree.
