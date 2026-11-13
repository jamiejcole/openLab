# Groups, GO, playhead

A Group is a cue like any other (pre-wait, post-wait, continue, operations, second trigger). Its action is running its children. Modes: `GroupMode` in `cue.rs`.

## The group's action
- Starts when the group's pre-wait elapses (`A`); children start at `A` per mode, each then runs its own pre-wait.
- A child is busy while it has a runtime that is not idle, loaded or settled broken: pre-waiting, running, post-waiting, paused, stopping, or broken with an auto-continue pending.
- The action completes on the sample its last busy child stops being busy (completes, settles broken, is stopped, reset, deleted). Then the group's continue applies: auto-follow triggers the cue after the group at that sample; auto-continue fires `A + post-wait` (longer than the children: the group is `post-waiting`).
- A group whose run saw a child break at run time does not auto-follow (its auto-continue still fires). Nested groups pass this up: an inner group that saw a break counts as a broken child of the outer. Exception: halted playlist.
- A child already broken when the group gets to it (static problems, or broken earlier and not reset) is skipped and does not stop the group from following.
- If the last busy child left by stop, reset or delete, the group stops too (idle, post-wait and follow cancelled) instead of completing. Stopping one child while another is busy only stops that child.
- An empty or disarmed group completes at once (a disarmed group starts no children). Feedback never concerns a group.

## Modes

| Mode | On action start | Children's continues | Playhead after GO on the group |
|---|---|---|---|
| `Timeline` | every child at `A` | ignored | after the group's continue chain |
| `StartFirstAndGoToNext` | first child | apply, chain within the group | after the group's chain |
| `StartFirstAndEnter` | first child | apply | into the group: after the first child's chain (disarmed: past the group) |
| `Playlist` | first child (shuffled: first of a pass) | ignored | after the group's chain |
| `StartRandom` | one random child | apply: the chosen child's continue starts the child after it | after the group's chain |

### Playlist
- Each next child starts on the sample the previous child's action completes (its pre-wait then counts).
- `StartFailed` child: skipped, the playlist moves on at once. A child that breaks while playing (`ActionFailed`, not `device_lost`) halts the playlist (below). `device_lost`, or a failure while stopped or panicked, leaves as a stopped child: no further child starts.
- A child broken when reached is skipped: static problems keep their timing (the next child starts when that has run); broken at run time and not reset: passed over at once.
- No loop: completes after the last child. Loop: wraps, never completes on its own (stop or devamp it).
- Shuffle: each pass plays every child once in a fresh order fixed at the pass start; removed children are skipped, added ones play from the next pass. A pass never opens with the child that closed the previous one (two or more children).
- Unshuffled: the next child is the one after the current as the group stands when the trigger fires (a reorder applies; a removed current child is followed by the one that took its place).
- A pass in which no child could start ends the playlist; so does the chain limit.
- Paused playlist: if the current child's end (or a failure before it played) arrives dated before the pause, the playlist moves on at the resume (next child on the resume sample; if it was the last, the group completes there). A failure while playing that arrives during the pause halts it as below, counted from the resume. A pause never ends a playlist early or skips children.

### Halted playlist
Trigger: the current child of a running playlist breaks while playing (`ActionFailed`, not `device_lost`, not while being stopped or panicked). The halt-and-follow rule is a measured QLab case plus decisions (skill `qlab-behaviour`).
- No further child starts. The action goes on, with no busy child, until the broken child's nominal end, then completes. It is `running` until then; `is_halted(cue)`.
- Nominal end = the child's known end (`ActionLength`), else action start + nominal length (`CueLengths`). If the broken child's own end is unknown the group completes at once, no follow.
- Auto-follow is kept. It fires on the sample the playlist would have ended on had every child played nominally: that end plus, for each child it would still have played (list order after the broken one; shuffled: rest of the pass), its pre-wait + nominal action.
- Nominal action: audio from `CueLengths`; Wait/Fade their duration; memo, control, Network 0; disarmed or static-problem child: pre-wait only; child broken at run time and not reset: nothing; nested timeline: longest child; nested non-looping playlist: sum of children.
- Follow is cancelled (group completes at the nominal end and stops) when the playlist loops, or any needed length is unknown or endless (unprobed or looping audio, nested looping playlist, nested random or start-first group).
- Between completion and follow the group shows `post-waiting` (no finished state with a pending deadline). Stop, Pause/Resume (the wait moves by the pause), Reset and Panic act on that wait as on a post-wait.
- Follow time is fixed when the halt starts; later edits and `CueLengths` do not move it. The group's own auto-continue keeps its schedule.
- A child started on its own during the halt is a busy child: stopping it stops the group and cancels the follow (QLab unknown).
- `PlayNext`/`PlayPrevious` on a halted playlist moves it on (halt ends, no follow). A devamped playlist is not halted: it completes at the break, no follow.
- A nested group that ends with a child broken at run time counts as a broken child of the playlist around it (that playlist moves on, does not follow), it does not halt it. A broken child paused on its own when it failed has its nominal end counted from its frozen deadline.

### Random (round-robin)
- Playable = armed, no static problems, not busy. Every playable child plays once before any plays again.
- Each action start picks uniformly (seeded generator) among playable children not yet picked this cycle, as the group stands then: a child added mid-cycle joins, a removed one drops out, a disarmed/broken/busy one is passed over while so and is a candidate again once playable and unpicked.
- No candidate left: new cycle, never opening with the child picked last (two or more playable). No child playable at all: the rule runs over all children, so one is always picked (its start then skips it).
- The cycle lives in the engine only: not saved, survives Stop/Reset/Panic, cleared by Reset All and a new engine.

## A child started on its own
Starting a cue inside a group (`Start`, GO on a child, a Start cue, a hotkey) does not make the group run. A group that is idle or loaded stays so (or `broken`); a group with a run keeps it (a running group counts the child as busy; a post-waiting one goes on post-waiting). Stop, Pause, Resume, Reset on the group, Panic, Pause All and Resume All still reach the child.
- Timeline, random, start-first: only that child plays, with its own continue where the mode applies children's continues (none in a timeline). The playhead goes to the next cue in the list.
- Playlist follow-through: when the child's action completes (or it breaks) the next child in list order starts on that sample, and so on. List order even with shuffle on. Children a start refuses at once are passed over (one round per call); static problems keep their timing. No loop: the last child ends it. Loop: wraps until stopped. The group fires no continue (it never ran).
- A child that ends while its playlist post-waits does not carry the playlist on; one ending after the post-wait does (not measured).
- Only a run that started as a child of that playlist, and is still its child, carries it on. A cue moved into a playlist while it runs, pre-waits or is held paused starts none of its children when it ends, and a running playlist does not move on for it. Its end is not held across Pause All for the playlist (assumed).
- Each follow-through start counts against the chain budget, so a looping follow-through of zero-length children is cut at the limit.
- A child started while its group pre-waits runs on its own; when the group's action starts it gets a second trigger and its option applies.
- A group fading out in a panic is the exception (`panic-transport.md`).

## State and operations of a group
- State comes from the group's own run: `pre-waiting`, `running` (any child busy), `post-waiting`, `paused`, `stopping` (stop with fade waits for children). With no run: `broken` while a descendant is shown with problems, else `idle`. A paused child alone does not pause its group.
- Each operation applies to the group and every descendant at any depth.
- Stop: every descendant with a runtime, children before groups, then the group. Hard: idle at once. With fade: each playing voice fades (silent actions stop at once); a group in its action with a child still fading is `stopping` until its last child stopped. A group not yet in its action (pre-waiting, loaded) stops at once and never starts children. A second stop is hard. A stopping group cannot be paused.
- Pause: the group and every active descendant (`PauseVoice`); timers keep their remaining time. Resume (also Start/GO on a paused group, whatever its option): the group and every paused descendant; a group whose last child left while paused ends then.
- Reset: every descendant (clears broken children) and the group.
- Load: the group and the children its action starts first: timeline all; start-first modes and unshuffled playlist the first; shuffled playlist and random none. Load to `t` loads them `max(t - group pre-wait, 0)` in. Past the nominal span: `timing.md`.
- Start on an active group applies the group's second-trigger option to the group as a whole.
- Nesting is bounded by `MAX_GROUP_DEPTH` (32, `validate.rs`), enforced by validation and commands. Engine walks are iterative; `Workspace::find_cue` / `tree::find_path` recurse, bounded by that depth in any validated workspace.

## Skip if disarmed
A cue is skipped when it is disarmed (`is_armed`, an Arm/Disarm cue's change in the call included) and has `skip_if_disarmed` (`Cue::skipped`, `CueEngine::is_skipped`). It is passed over as if absent (QLab 5, manual p. 128); armed, the flag does nothing.
- A trigger (GO, Start cue, hotkey, OSC, continue) on it starts nothing: no run, no `pre-waiting`, no warning, no chain budget; its own continue never fires. A run it already had (disarmed since) keeps rule 1. Load of it does nothing (an auto-load passes over it).
- A continue starts the next sibling that is there (`next_sibling`); with none, nothing. A chain `chain_end` walks the cues that are there; the playhead goes to the next position that is there at that level (a skipped group with its children), else up a level, else past the end.
- GO with the playhead put on a skipped cue starts nothing and moves the playhead to the next cue that is there (assumed, skill `qlab-behaviour`).
- Groups: timeline never starts it; start-first modes start the first child that is there (the playhead enters after its chain); a playlist passes over it at once (no timing kept); a random group never picks it, also when nothing else is playable. Its nominal span is 0. A group's Load loads the first children that are there.
- Navigation (`PlayheadNext`/`Previous`, `SetPlayhead`) can still stand on it; only GO and chains pass over it.

## GO and double-GO (PB-01, PB-02)
- `Go { list }` starts the cue at the list's playhead at `now` and moves the playhead to the cue after the last cue of its continue chain (every following cue reached through auto-continue/auto-follow, as the model stands, is skipped). A chain to the list's end puts the playhead past the end.
- Only an armed `StartFirstAndEnter` group is entered: the playhead goes to the child after the first child's chain (a disarmed one starts nothing and is skipped as a unit). Every other group is one unit: GO on it, or a chain reaching it, moves the playhead after the group's chain, never onto its children.
- GO on a cue inside a group starts that cue only; its groups do not run. The playhead moves past its continue chain among its siblings (children's continues chain in start-first and random groups, are ignored in timeline). In a playlist the rest of the children play on, so the playhead leaves the group at once. Past the last child it leaves the group, and so on outwards.
- Leaving a group whose continue fires or has fired (it has a run, or its run fired its continue during this visit of the playhead) goes to the cue after the group's continue chain. A visit ends when the playhead is inside the group any other way (`SetPlayhead`, `SetPlayheadByNumber`, `PlayheadNext`/`Previous` from outside, a GoTo, an edit moving the playhead cue in or deleting its cue), so what a group fired earlier never hides its targets.
- The engine compares the groups around the playhead with those of its last look (after every move and followed edit): an edit leaving the playhead in the same groups keeps the visit.
- Leaving a group with no run that fired nothing (usual after GO on a child) stands on the cue right after the group, so its continue targets are not lost. A playhead put explicitly on a child of a group GO does not enter stays among its children, GO by GO, until it leaves (same path as `PlayheadNext`).
- GO on an active cue applies its second-trigger option (default `TriggerIgnored`); the GO counts and the playhead moves. GO on a paused cue resumes it. GO on a broken cue does not start it (`CueBroken`) but counts and moves the playhead on. A cue with static problems keeps its timing, so its continue, an auto-follow too, fires.
- `GoIgnored { reason }`: `no_cue` (past the end or empty list), `unknown_list`, `double_go`, `chain_limit`.
- GoTo-and-GO is not an operator GO: double-GO never blocks it and it does not restart the interval.
- Double-GO: a GO less than `min_go_interval` after the previous accepted GO (samples, any list) is ignored; exactly at the interval is accepted; ignored GOs do not restart it; 0 = off; if `now` is earlier than the last accepted GO (clock restart) the GO is accepted. Read at each GO. Panic does not touch it.
- `Input::GoFrom { list, cue }` moves the playhead to `cue` and GOes as one input; a cue not a position of the list warns `NotInList` and does nothing. A GO that double-GO refuses leaves the playhead where it was.

## Playhead (PB-05, CL-06)
- Each list has a playhead: the standing-by cue, or `None` past the end or for an empty list. A new list stands on its first cue. `PlayheadChanged` reports every move, including the initial one.
- Positions: the playhead can stand on any cue at any depth in a group of any mode. `playhead_positions(ws, list)` lists them depth-first, a group before its children (`1, 2, 2.1, 2.2, 3`). With duplicate ids the first occurrence counts.
- Stepping: `PlayheadNext`/`PlayheadPrevious` step through positions leaving out the children of every group except `StartFirstAndEnter` (entered by mode alone, also when disarmed: navigation fires nothing). Other groups are passed over as a unit, both ways. Exception: a group the playhead already stands inside: its children are stepped through, then it leaves the group, as GO from a child does.
- `PlayheadNext`: one step down (enter group to its first child; a group's last child to the cue after the group); the last step goes past the end, which stays. `PlayheadPrevious`: one step up (from the cue after an enter group to its last reachable descendant; from the cue after any other group to the group itself); from past the end to the last step; the first stays.
- `SetPlayhead { cue }` (`None` = past the end) and `SetPlayheadByNumber` (exact number, first in position order) take cues of that list; otherwise `NotInList` / `NumberNotFound` and the playhead stays. Navigation never fires anything.
- Edits: a reordered or moved playhead cue still in the list keeps the playhead. A deleted one (or inside a deleted group) gives way to the first position after it that still exists, or past the end. An empty list that gains cues stands on its first. A deleted list loses its playhead.
