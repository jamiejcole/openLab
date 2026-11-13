// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Level, mute and routing edits on a playing audio cue (AUD-06, AUD-08, NFR-01),
//! through the control loop and rendered offline: the edit is heard within the 10 ms edit ramp
//! (80 frames at 8 kHz), with no step in the samples, no voice restart, and fades start from
//! what is heard.

use crate::show::*;
use openlab_core::protocol::{ClientMessage, Outcome, ServerMessage, TransportAction};
use openlab_core::{
    Command, Crosspoint, CueKind, CueLevels, CueProperty, CueState, FadeCurve, FadeLevels,
    GroupMode, OutputPatch, PatchId, PatchMapping, PatchOutput, db_to_gain,
};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;

/// The edit ramp at [`RATE`].
const EDIT: usize = 80;
const LEVEL: f32 = 0.5;

/// Sends `SetCueProperty Kind` for cue `n` with its levels changed by `f`, as the Levels tab
/// does (a drag's edits share one coalesce key). Returns the sample the edit is dated on.
fn set_levels(s: &mut OfflineShow, n: u128, f: impl FnOnce(&mut CueLevels)) -> usize {
    set_audio(s, n, |_, levels| f(levels))
}

/// As [`set_levels`], with the cue's patch to change too.
fn set_audio(s: &mut OfflineShow, n: u128, f: impl FnOnce(&mut PatchId, &mut CueLevels)) -> usize {
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(n))
        .unwrap()
        .kind
        .clone();
    let CueKind::Audio { patch, levels, .. } = &mut kind else {
        panic!("not audio");
    };
    f(patch, levels);
    send_edit(
        s,
        Command::SetCueProperty {
            id: id(n),
            property: CueProperty::Kind { value: kind },
        },
    )
}

/// Sends `command` (with the Levels tab's coalesce key) and asserts it was not refused.
/// Returns the sample the edit is dated on.
fn send_edit(s: &mut OfflineShow, command: Command) -> usize {
    let at = s.now() as usize;
    let rid = s.request_id();
    let replies = s.send(ClientMessage::Command {
        id: rid,
        command: Box::new(command),
        coalesce: Some("levels".into()),
    });
    assert!(
        replies.iter().all(|m| !matches!(
            m,
            ServerMessage::Result {
                outcome: Outcome::Error(_),
                ..
            }
        )),
        "{replies:?}"
    );
    assert_eq!(s.control().engine().unsignalled_model_changes(), 0);
    at
}

/// The largest step between neighbouring samples.
fn max_jump(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// A step a linear ramp of `EDIT` frames from `from` to `to` may take per sample (with slack
/// for rounding); a click is a whole `|to − from|` in one sample.
fn ramp_step(from: f32, to: f32) -> f32 {
    (to - from).abs() / EDIT as f32 * 1.5 + 1e-6
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5
}

/// Asserts channel `ch` holds `from` up to the edit at `at`, ramps to `to` over the edit ramp
/// without a step, then holds `to` up to `until`.
fn heard_edit(s: &OfflineShow, ch: usize, at: usize, from: f32, to: f32, until: usize) {
    let x = s.channel(ch);
    assert!(x[at - 100..at].iter().all(|&v| near(v, from)), "before");
    let mid = x[at + EDIT / 2];
    assert!(near(mid, (from + to) / 2.0), "mid {mid}, {from} → {to}");
    assert!(x[at + EDIT..until].iter().all(|&v| near(v, to)), "after");
    let jump = max_jump(&x[at - 100..until]);
    assert!(jump <= ramp_step(from, to), "jump {jump}");
}

fn long_show(extra: Vec<openlab_core::Cue>) -> OfflineShow {
    let a = wav(&dc(RATE as usize * 10, LEVEL));
    let mut cues = vec![audio(1, &a, 0)];
    cues.extend(extra);
    show(workspace(cues))
}

#[test]
fn master_output_crosspoint_and_mute_edits_are_heard_at_once_without_a_click() {
    let mut s = long_show(vec![]);
    go(&mut s);
    s.run(800);
    s.take_outbound();
    let g6 = LEVEL * db_to_gain(-6.0);
    let at = set_levels(&mut s, 1, |l| l.master_db = -6.0);
    s.run(400);
    heard_edit(&s, 0, at, LEVEL, g6, at + 300);

    // A cue output.
    let g18 = LEVEL * db_to_gain(-18.0);
    let at = set_levels(&mut s, 1, |l| l.outputs[0].db = -12.0);
    s.run(400);
    heard_edit(&s, 0, at, g6, g18, at + 300);

    // A crosspoint routes the file to output 1 as well (AUD-06).
    let at = set_levels(&mut s, 1, |l| {
        l.crosspoints.push(Crosspoint {
            input: 0,
            output: 1,
            db: -6.0,
            mute: false,
        });
    });
    s.run(400);
    let to = LEVEL * db_to_gain(-12.0);
    heard_edit(&s, 1, at, 0.0, to, at + 300);

    // A mute.
    let at = set_levels(&mut s, 1, |l| l.outputs[0].mute = true);
    s.run(400);
    heard_edit(&s, 0, at, g18, 0.0, at + 300);

    // The voice played on throughout: no restart, no state change (NFR-01).
    assert_eq!(s.control().live_voices(), 1);
    assert!(states(&s.take_outbound(), id(1)).is_empty());
}

#[test]
fn a_fader_drag_of_100_edits_in_a_second_is_smooth_and_ends_on_the_last() {
    let mut s = long_show(vec![]);
    go(&mut s);
    s.run(800);
    s.take_outbound();
    let first = s.now() as usize;
    for k in 1..=100u32 {
        set_levels(&mut s, 1, |l| l.master_db = -0.2 * f64::from(k));
        s.run(RATE as u64 / 100);
    }
    s.run(400);
    let x = s.channel(0);
    let end = LEVEL * db_to_gain(-20.0);
    let last = x.len() - 1;
    assert!(near(x[last], end), "{}", x[last]);
    // Each edit moves 0.2 dB: never more than one edit's ramp step per sample.
    let jump = max_jump(&x[first - 100..]);
    assert!(jump <= ramp_step(LEVEL, LEVEL * db_to_gain(-0.2)), "{jump}");
    assert_eq!(s.control().live_voices(), 1);
    assert!(states(&s.take_outbound(), id(1)).is_empty());
}

fn master_fade(n: u128, target: u128, frames: u64, db: f64) -> openlab_core::Cue {
    cue(
        n,
        CueKind::Fade {
            target: Some(id(target)),
            duration: secs(frames),
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(db),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
        },
    )
}

/// QLab 5.5.3: an edit of the master a running Fade is moving is heard, and ends the Fade: the
/// room hears the edited level from the edit ramp on, not the rest of the fade.
#[test]
fn a_master_edit_during_a_fade_ends_the_fade_and_is_heard() {
    let mut s = long_show(vec![master_fade(2, 1, 8000, -20.0)]);
    start(&mut s, 1);
    s.run(800);
    start(&mut s, 2);
    s.run(400);
    assert_eq!(s.control().engine().state(id(2)), CueState::Running);
    let at = set_levels(&mut s, 1, |l| l.master_db = -6.0);
    s.run(1600);
    assert_eq!(s.control().engine().state(id(2)), CueState::Idle);
    assert_eq!(s.control().engine().state(id(1)), CueState::Running);
    let x = s.channel(0);
    let g6 = LEVEL * db_to_gain(-6.0);
    assert!(x[at + EDIT..].iter().all(|&v| near(v, g6)));
    assert!(max_jump(&x[at - 100..]) <= ramp_step(LEVEL, g6));

    // A mute is heard the same way.
    let mut s = long_show(vec![master_fade(2, 1, 8000, -20.0)]);
    start(&mut s, 1);
    s.run(800);
    start(&mut s, 2);
    s.run(400);
    let at = set_levels(&mut s, 1, |l| l.crosspoints[0].mute = true);
    s.run(400);
    let x = s.channel(0);
    assert!(x[at + EDIT..].iter().all(|&v| v == 0.0));
    assert!(max_jump(&x[at - 100..]) <= ramp_step(LEVEL, 0.0));
}

#[test]
fn a_paused_cue_resumes_at_the_edited_level_without_a_jump() {
    let mut s = long_show(vec![]);
    go(&mut s);
    s.run(800);
    s.transport(TransportAction::Pause { cue: id(1) });
    s.run(400);
    set_levels(&mut s, 1, |l| l.master_db = -6.0);
    s.run(400);
    let resume = s.now() as usize;
    s.transport(TransportAction::Resume { cue: id(1) });
    s.run(800);
    let x = s.channel(0);
    // Silent while paused (past the pause's declick tail).
    assert!(x[resume - 100..resume].iter().all(|&v| v == 0.0));
    let g6 = LEVEL * db_to_gain(-6.0);
    assert!(x[resume + 200..].iter().all(|&v| near(v, g6)));
    // The edit is heard over its ramp from the resume, not as a jump to the new level: the
    // resumed voice starts from the level it paused at, under the resume's 5 ms declick
    // fade-in (40 frames), and is still on its way down when that ends.
    let after = &x[resume..resume + 200];
    let first = after.iter().position(|&v| v != 0.0).unwrap();
    let declick = 40;
    assert!(
        after[first + declick] > g6 + 0.05,
        "{}",
        after[first + declick]
    );
    assert!(max_jump(&after[first..]) <= ramp_step(0.0, LEVEL) * 2.0);
    assert!(max_jump(&after[first + declick..]) <= ramp_step(LEVEL, g6));
}

#[test]
fn a_fade_after_an_edit_starts_from_the_heard_level() {
    let mut s = long_show(vec![master_fade(2, 1, 800, -20.0)]);
    start(&mut s, 1);
    s.run(800);
    set_levels(&mut s, 1, |l| l.master_db = -6.0);
    s.run(400);
    let tf = start(&mut s, 2) as usize;
    s.run(1200);
    let x = s.channel(0);
    let (from, to) = (LEVEL * db_to_gain(-6.0), LEVEL * db_to_gain(-20.0));
    assert!(near(x[tf - 1], from));
    assert!(near(x[tf + 400], (from + to) / 2.0), "{}", x[tf + 400]);
    assert!(near(x[tf + 900], to));
    assert!(max_jump(&x[tf - 100..tf + 900]) <= (from - to) / 800.0 * 1.5);
}

#[test]
fn a_relative_group_fade_after_an_edit_moves_the_edited_level() {
    let a = wav(&dc(RATE as usize * 10, LEVEL));
    let group = cue(
        1,
        CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![audio(3, &a, 0)],
        },
    );
    let mut s = show(workspace(vec![group, master_fade(2, 1, 800, -10.0)]));
    start(&mut s, 1);
    s.run(800);
    set_levels(&mut s, 3, |l| l.master_db = -6.0);
    s.run(400);
    let tf = start(&mut s, 2) as usize;
    s.run(1200);
    let x = s.channel(0);
    assert!(near(x[tf - 1], LEVEL * db_to_gain(-6.0)));
    // −6 dB moved by −10 dB.
    assert!(
        near(x[tf + 900], LEVEL * db_to_gain(-16.0)),
        "{}",
        x[tf + 900]
    );
}

#[test]
fn an_edit_to_an_idle_cue_changes_nothing_until_it_starts_with_it() {
    let mut s = long_show(vec![]);
    set_levels(&mut s, 1, |l| l.master_db = -6.0);
    s.run(400);
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
    assert_eq!(s.control().live_voices(), 0);
    let at = go(&mut s) as usize;
    s.run(400);
    // It starts at the edited level on its first sample, with no ramp.
    let x = s.channel(0);
    assert!(x[at..].iter().all(|&v| near(v, LEVEL * db_to_gain(-6.0))));
}

// ----- a patch change of a playing cue -----

/// The mixer's de-zipper at [`RATE`] (5 ms): a moved voice crossfades over it.
const MOVE: usize = 40;

/// A second patch, "Side": one output, mapped to device output 1.
fn side() -> PatchId {
    PatchId(uuid::Uuid::from_u128(902))
}

/// [`long_show`] with the "Side" patch as well, cue 1 on `patch` routed to cue output `out`.
fn two_patch_show(patch: PatchId, out: u32, extra: Vec<openlab_core::Cue>) -> OfflineShow {
    two_patch_show_with(patch, out, extra, |_, _| {})
}

/// [`two_patch_show`], with `tweak` applied to the workspace and the config before it starts.
fn two_patch_show_with(
    patch: PatchId,
    out: u32,
    extra: Vec<openlab_core::Cue>,
    tweak: impl FnOnce(&mut openlab_core::Workspace, &mut ControlConfig),
) -> OfflineShow {
    let a = wav(&dc(RATE as usize * 10, LEVEL));
    let mut c = audio(1, &a, out);
    if let CueKind::Audio { patch: p, .. } = &mut c.kind {
        *p = patch;
    }
    let mut cues = vec![c];
    cues.extend(extra);
    let mut ws = workspace(cues);
    ws.patches.push(OutputPatch {
        id: side(),
        name: "Side".into(),
        master_db: 0.0,
        outputs: vec![
            PatchOutput {
                level_db: 0.0,
                mute: false,
                polarity_inverted: false,
            };
            1
        ],
    });
    let mut c = config(&ws);
    c.machine
        .set_patch_mapping(
            ws.id,
            side(),
            PatchMapping {
                device: "Offline".into(),
                channels: vec![1],
            },
        )
        .unwrap();
    tweak(&mut ws, &mut c);
    show_with(ws, c)
}

/// Asserts the sound moves from device output `from` to `to` at `at`: whole on `from` before,
/// a crossfade over the de-zipper that always adds up to the signal, whole on `to` after.
fn heard_move(s: &OfflineShow, from: usize, to: usize, at: usize, until: usize) {
    let (a, b) = (s.channel(from), s.channel(to));
    assert!(a[at - 100..at].iter().all(|&v| near(v, LEVEL)), "before");
    assert!(b[at - 100..at].iter().all(|&v| v == 0.0), "before");
    for (x, y) in a[at - 100..until].iter().zip(&b[at - 100..until]) {
        assert!(near(x + y, LEVEL), "{x} + {y}");
    }
    assert!(a[at + MOVE..until].iter().all(|&v| v == 0.0), "after");
    assert!(b[at + MOVE..until].iter().all(|&v| near(v, LEVEL)), "after");
    for x in [&a, &b] {
        let jump = max_jump(&x[at - 100..until]);
        assert!(jump <= LEVEL / MOVE as f32 * 1.5 + 1e-6, "jump {jump}");
    }
}

/// An operator switches a playing cue to another patch: it is heard on the new patch's
/// outputs at once (a 5 ms crossfade, no click), with no restart, no new voice and no state
/// change, and back again.
#[test]
fn a_playing_cue_follows_its_patch_change_without_a_click() {
    let mut s = two_patch_show(patch_id(), 0, vec![]);
    go(&mut s);
    s.run(800);
    s.take_outbound();
    let at = set_audio(&mut s, 1, |p, _| *p = side());
    s.run(400);
    heard_move(&s, 0, 1, at, at + 300);
    let at = set_audio(&mut s, 1, |p, _| *p = patch_id());
    s.run(400);
    heard_move(&s, 1, 0, at, at + 300);
    assert_eq!(s.control().live_voices(), 1);
    assert!(states(&s.take_outbound(), id(1)).is_empty());
}

/// A playing cue on a 2-output patch with output 4 muted (or not in its level list at all) and a
/// crosspoint to it, moved to a 4-output patch: output 4 stays silent. The move sends levels only
/// for the outputs it adds that are heard, so every cue output the voice did not start with
/// must begin silent in the mixer.
fn moved_to_four_outputs(levels_of: impl FnOnce(&mut CueLevels)) -> f32 {
    let mut s = two_patch_show_with(patch_id(), 0, vec![], |ws, c| {
        ws.patches[1].outputs = vec![ws.patches[1].outputs[0]; 4];
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels_of(levels);
            levels.crosspoints.push(Crosspoint {
                input: 0,
                output: 3,
                db: 0.0,
                mute: false,
            });
        }
        c.machine
            .set_patch_mapping(
                ws.id,
                side(),
                PatchMapping {
                    device: "Offline".into(),
                    channels: vec![1; 4],
                },
            )
            .unwrap();
    });
    go(&mut s);
    s.run(800);
    set_audio(&mut s, 1, |p, _| *p = side());
    s.run(800);
    *s.channel(1).last().unwrap()
}

#[test]
fn a_muted_cue_output_stays_silent_when_the_playing_cue_moves_to_a_patch_that_has_it() {
    let heard = moved_to_four_outputs(|levels| {
        levels.outputs = vec![levels.outputs[0]; 4];
        levels.outputs[3].mute = true;
    });
    assert!(
        near(heard, LEVEL),
        "only output 1 is heard: {heard}, want {LEVEL}"
    );
}

#[test]
fn an_unlisted_cue_output_stays_silent_when_the_playing_cue_moves_to_a_patch_that_has_it() {
    let heard = moved_to_four_outputs(|levels| levels.outputs.truncate(1));
    assert!(
        near(heard, LEVEL),
        "only output 1 is heard: {heard}, want {LEVEL}"
    );
}

/// A playing cue moved from a 2-output patch to a 100-output one is heard on the new patch's
/// later outputs too: the voice started 2 cue outputs wide (what its patch had) and the move's
/// level ramps widen it to output 100.
#[test]
fn a_playing_cue_moved_to_a_wider_patch_is_heard_on_its_later_outputs() {
    let mut s = two_patch_show_with(patch_id(), 0, vec![], |ws, c| {
        ws.patches[1].outputs = vec![ws.patches[1].outputs[0]; 100];
        if let CueKind::Audio { levels, .. } = &mut ws.cue_lists[0].cues[0].kind {
            levels.outputs = vec![levels.outputs[0]; 100];
            levels.crosspoints.push(Crosspoint {
                input: 0,
                output: 99,
                db: 0.0,
                mute: false,
            });
        }
        c.machine
            .set_patch_mapping(
                ws.id,
                side(),
                PatchMapping {
                    device: "Offline".into(),
                    channels: vec![1; 100],
                },
            )
            .unwrap();
    });
    go(&mut s);
    s.run(800);
    // On Main only its output 1 is heard (output 100 is beyond Main's two).
    let ch0 = s.channel(0);
    assert!(near(*ch0.last().unwrap(), LEVEL));
    let at = set_audio(&mut s, 1, |p, _| *p = side());
    s.run(800);
    let ch1 = s.channel(1);
    assert!(
        ch1[at + MOVE + 10..].iter().all(|&v| near(v, 2.0 * LEVEL)),
        "outputs 1 and 100 of Side both on device output 2"
    );
    assert_eq!(s.control().live_voices(), 1);
}

/// #346: the slot a voice moved off keeps its routing while the crossfade out of it plays,
/// even when its patch is removed by the same edit. Buffers shorter than the 5 ms crossfade
/// (as 64 frames at 48 kHz are) let a sync run during it: the slot must not be zeroed then.
#[test]
fn a_slot_a_move_left_keeps_its_routing_until_the_crossfade_ends() {
    let mut s = two_patch_show_with(patch_id(), 0, vec![], |_, c| c.block_frames = 16);
    go(&mut s);
    s.run(800);
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .clone();
    if let CueKind::Audio { patch, .. } = &mut kind {
        *patch = side();
    }
    let at = send_edit(
        &mut s,
        Command::Transaction {
            commands: vec![
                Command::SetCueProperty {
                    id: id(1),
                    property: CueProperty::Kind { value: kind },
                },
                Command::RemovePatch {
                    id: patch_id(),
                    even_if_used: false,
                },
            ],
        },
    );
    s.run(400);
    heard_move(&s, 0, 1, at, at + 300);
    assert_eq!(s.control().live_voices(), 1);
}

/// #346: the slot a playing cue moved off is free for another patch once the crossfade out of
/// it has ended, not only when the cue stops; never before it ends (its tail would be heard on
/// the new patch's outputs). Two mixer slots, Main's and Third's; Side waits for one.
#[test]
fn a_slot_a_move_left_is_handed_on_once_the_crossfade_ends() {
    let b = wav(&dc(RATE as usize, LEVEL));
    let mut on_side = audio(2, &b, 0);
    if let CueKind::Audio { patch, .. } = &mut on_side.kind {
        *patch = side();
    }
    let mut s = two_patch_show_with(patch_id(), 0, vec![on_side], |ws, c| {
        let mut p = ws.patches[1].clone();
        p.id = third();
        p.name = "Third".into();
        ws.patches.insert(1, p);
        c.block_frames = 16;
    });
    go(&mut s);
    s.run(800);
    let mut kind = s
        .control()
        .workspace()
        .find_cue(id(1))
        .unwrap()
        .kind
        .clone();
    if let CueKind::Audio { patch, .. } = &mut kind {
        *patch = third();
    }
    let at = send_edit(
        &mut s,
        Command::Transaction {
            commands: vec![
                Command::SetCueProperty {
                    id: id(1),
                    property: CueProperty::Kind { value: kind },
                },
                Command::RemovePatch {
                    id: patch_id(),
                    even_if_used: false,
                },
            ],
        },
    );
    s.run(200);
    // Third's output is Main's: cue 1 is heard on through the move, and never on Side's.
    assert!(s.channel(0)[at - 100..].iter().all(|&v| near(v, LEVEL)));
    assert!(s.channel(1).iter().all(|&v| v == 0.0), "tail on Side");
    let started = start(&mut s, 2) as usize;
    s.run(400);
    let x = s.channel(1);
    assert!(
        x[started + MOVE..].iter().all(|&v| near(v, LEVEL)),
        "Side got no slot while cue 1 plays"
    );
    assert_eq!(s.control().live_voices(), 2);
}

/// #347: a preview keeps the patch it started on when its cue's patch changes, as it keeps its
/// levels: the audition is not moved onto the show's outputs.
#[test]
fn a_preview_keeps_its_patch_when_the_cue_changes_patch() {
    let mut s = two_patch_show(patch_id(), 0, vec![]);
    s.transport(TransportAction::Preview { cue: id(1) });
    s.run(800);
    let at = set_audio(&mut s, 1, |p, _| *p = side());
    s.run(400);
    let (main, side_out) = (s.channel(0), s.channel(1));
    assert!(
        main[at - 100..].iter().all(|&v| near(v, LEVEL)),
        "moved off"
    );
    assert!(side_out.iter().all(|&v| v == 0.0), "heard on the new patch");
}

/// The third patch of [`with_third`].
fn third() -> PatchId {
    PatchId(uuid::Uuid::from_u128(903))
}

/// [`two_patch_show_with`] (cue 1 on Main) with a one-output patch "Third" listed between Main
/// and Side and left to the default mapping (device output 1, as Main's first output). The
/// mixer has two slots: Main and Third take them, so Side has none.
fn with_third(extra: Vec<openlab_core::Cue>) -> OfflineShow {
    two_patch_show_with(patch_id(), 0, extra, |ws, _| {
        let mut p = ws.patches[1].clone();
        p.id = third();
        p.name = "Third".into();
        ws.patches.insert(1, p);
    })
}

/// A slot freed by one patch and handed to another takes the new patch's routing at once: a
/// cue started on it on the same sample is heard on its own patch's outputs only, from its
/// first sample, never for a moment on the outputs of the patch that had the slot before.
#[test]
fn a_slot_handed_to_another_patch_takes_its_routing_at_once() {
    let b = wav(&dc(RATE as usize, LEVEL));
    let mut on_side = audio(2, &b, 0);
    if let CueKind::Audio { patch, .. } = &mut on_side.kind {
        *patch = side();
    }
    let mut s = with_third(vec![on_side]);
    s.run(800);
    let at = send_edit(
        &mut s,
        Command::RemovePatch {
            id: third(),
            even_if_used: false,
        },
    );
    start(&mut s, 2);
    s.run(400);
    assert!(
        s.channel(0).iter().all(|&v| v == 0.0),
        "heard on Third's output"
    );
    let x = s.channel(1);
    assert!(x[..at].iter().all(|&v| v == 0.0));
    assert!(
        x[at..at + 300].iter().all(|&v| near(v, LEVEL)),
        "ramped in: {:?}",
        &x[at..at + 4]
    );
}

/// #347: a cue changed to a patch with no free slot keeps playing where it is; once a slot
/// frees, the next sync gives the patch that slot and moves the voice onto it.
#[test]
fn a_cue_whose_new_patch_has_no_slot_moves_once_one_frees() {
    let mut s = with_third(vec![]);
    go(&mut s);
    s.run(800);
    let at = set_audio(&mut s, 1, |p, _| *p = side());
    s.run(400);
    assert!(
        s.channel(0)[at - 100..].iter().all(|&v| near(v, LEVEL)),
        "left its outputs without a slot to go to"
    );
    assert!(s.channel(1).iter().all(|&v| v == 0.0));
    let at = send_edit(
        &mut s,
        Command::RemovePatch {
            id: third(),
            even_if_used: false,
        },
    );
    s.run(400);
    heard_move(&s, 0, 1, at, at + 300);
    assert_eq!(s.control().live_voices(), 1);
}

/// A cue started on a one-output patch, routed to cue output 2 (silent there), moved to a
/// two-output patch: cue output 2 comes up at its level from the move.
#[test]
fn outputs_the_new_patch_adds_are_heard_at_their_levels() {
    let mut s = two_patch_show(side(), 1, vec![]);
    go(&mut s);
    s.run(800);
    assert!(s.channel(0).iter().chain(&s.channel(1)).all(|&v| v == 0.0));
    let at = set_audio(&mut s, 1, |p, _| *p = patch_id());
    s.run(400);
    let x = s.channel(1);
    assert!(
        x[at + MOVE..].iter().all(|&v| near(v, LEVEL)),
        "{}",
        x[at + MOVE]
    );
    // The cue output's level and its crosspoint come up over the de-zipper under the crossfade
    // (all three 5 ms from 0, so at most three times a crossfade's step): no click.
    let jump = max_jump(&x);
    assert!(jump <= 3.0 * LEVEL / MOVE as f32 * 1.1, "jump {jump}");
    assert!(s.channel(0).iter().all(|&v| v == 0.0));
}

/// A level a Fade cue set on an output the old patch did not have is kept by the move: the
/// voice already had the fade's value, the model's level does not replace it.
#[test]
fn a_faded_output_keeps_the_fade_level_when_its_patch_adds_it() {
    let fade = cue(
        2,
        CueKind::Fade {
            target: Some(id(1)),
            duration: secs(400),
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: None,
                output_db: vec![None, Some(-6.0)],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: Default::default(),
        },
    );
    let mut s = two_patch_show(side(), 1, vec![fade]);
    start(&mut s, 1);
    s.run(400);
    start(&mut s, 2);
    s.run(800);
    let at = set_audio(&mut s, 1, |p, _| *p = patch_id());
    s.run(400);
    let x = s.channel(1);
    let faded = LEVEL * db_to_gain(-6.0);
    assert!(
        x[at + MOVE..].iter().all(|&v| near(v, faded)),
        "{}",
        x[at + MOVE]
    );
}

/// "Fade and stop others", peers, 1 s: the cue playing beside it is heard fading out from its
/// level along a straight line over that second, and is silent and stopped after it.
#[test]
fn fade_and_stop_others_fades_a_playing_peer_out_over_its_time_and_stops_it() {
    let path = wav(&dc(RATE as usize * 4, LEVEL));
    let mut stopper = cue(2, CueKind::Memo);
    stopper.fade_and_stop_others = openlab_core::FadeAndStopOthers {
        enabled: true,
        scope: openlab_core::OthersScope::Peers,
        duration: 1.0,
    };
    let mut s = show(workspace(vec![audio(1, &path, 0), stopper]));
    start(&mut s, 1);
    s.run(BLOCK * 4);
    let at = start(&mut s, 2) as usize;
    s.run(u64::from(RATE) * 2);
    let out = s.channel(0);
    let near = |i: usize, want: f32| {
        assert!(
            (out[i] - want).abs() < 0.01,
            "sample {i}: {} not {want}",
            out[i]
        );
    };
    near(at - 1, LEVEL);
    near(at + RATE as usize / 4, LEVEL * 0.75);
    near(at + RATE as usize / 2, LEVEL * 0.5);
    near(at + RATE as usize * 3 / 4, LEVEL * 0.25);
    assert!(
        out[at + RATE as usize..].iter().all(|&v| v == 0.0),
        "silent after the fade"
    );
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    assert_eq!(s.control().live_voices(), 0);
}

/// "Duck others" -12 dB over 1 s, rendered: the cue playing beside it goes down to 12 dB lower
/// along the ramp, stays there while the ducking cue runs, and comes back over the same second
/// after it ends, never stepping; a cue started during the duck is heard ducked from its first
/// sample.
#[test]
fn duck_others_minus_12_db_is_heard_reached_held_and_returned_without_a_step() {
    let path = wav(&dc(RATE as usize * 6, LEVEL));
    let late = wav(&dc(RATE as usize, LEVEL));
    let mut ducker = cue(2, CueKind::Wait { duration: 2.0 });
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -12.0,
        duration: 1.0,
    };
    let mut s = show(workspace(vec![
        audio(1, &path, 0),
        ducker,
        audio(3, &late, 1),
    ]));
    start(&mut s, 1);
    s.run(BLOCK * 4);
    let at = start(&mut s, 2) as usize;
    s.run(u64::from(RATE) * 3 / 2);
    let second = start(&mut s, 3) as usize;
    s.run(u64::from(RATE) * 4);
    let out = s.channel(0);
    let ducked = LEVEL * db_to_gain(-12.0);
    let near = |x: &[f32], i: usize, want: f32| {
        assert!(
            (x[i] - want).abs() < 2e-3,
            "sample {i}: {} not {want}",
            x[i]
        );
    };
    let rate = RATE as usize;
    // Half way in dB, down and back up.
    let g = db_to_gain(-12.0);
    let half = |from: f32, to: f32| LEVEL * from * (to / from).sqrt();
    near(&out, at - 1, LEVEL);
    near(&out, at + rate / 2, half(1.0, g));
    near(&out, at + rate + 10, ducked);
    // The Wait ends at 2 s: back over 1 s.
    near(&out, at + 2 * rate + rate / 2, half(g, 1.0));
    near(&out, at + 3 * rate + 10, LEVEL);
    let jump = out[at - 10..at + 4 * rate]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(jump < 1e-3, "a step of {jump}");
    let late_out = s.channel(1);
    near(&late_out, second, ducked);
    near(&late_out, second + 100, ducked);
}

/// Panic during a duck: everything fades out, and the next run plays at its own level.
#[test]
fn panic_during_a_duck_leaves_no_ducked_level_on_the_next_run() {
    let path = wav(&dc(RATE as usize * 8, LEVEL));
    let mut ducker = cue(2, CueKind::Wait { duration: 5.0 });
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -20.0,
        duration: 0.5,
    };
    let mut ws = workspace(vec![audio(1, &path, 0), ducker]);
    ws.settings.panic_duration = 0.5;
    let mut s = show(ws);
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(u64::from(RATE));
    s.transport(TransportAction::Panic);
    s.run(u64::from(RATE));
    assert_eq!(s.control().engine().state(id(1)), CueState::Idle);
    let again = start(&mut s, 1) as usize;
    s.run(u64::from(RATE) / 2);
    let out = s.channel(0);
    assert!(
        (out[again + 10] - LEVEL).abs() < 1e-3,
        "{}",
        out[again + 10]
    );
}

/// The largest rise from one sample to the next from `from` on, as a ratio in dB of the level
/// at `from` (0 when it never rises).
fn rise_after(out: &[f32], from: usize) -> f32 {
    let mut worst = 0.0f32;
    for w in out[from..].windows(2) {
        if w[1] > w[0] + 1e-7 {
            worst = worst.max(w[1] - w[0]);
        }
    }
    worst
}

/// A ducking Wait and a playing cue, `ducker_first` saying which is listed first.
fn ducked_show(ducker_first: bool, wait_s: f64, db: f64, time: f64) -> OfflineShow {
    let path = wav(&dc(RATE as usize * 10, LEVEL));
    let mut ducker = cue(2, CueKind::Wait { duration: wait_s });
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db,
        duration: time,
    };
    let music = audio(1, &path, 0);
    let cues = if ducker_first {
        vec![ducker, music]
    } else {
        vec![music, ducker]
    };
    let mut ws = workspace(cues);
    ws.settings.panic_duration = 1.0;
    let mut s = show(ws);
    start(&mut s, 1);
    start(&mut s, 2);
    s
}

/// Panic during a -40 dB duck: the ducked cue only goes down from the panic on, whichever of
/// the two cues is listed first.
#[test]
fn panic_during_a_duck_never_brings_the_ducked_cue_up_whatever_the_cue_order() {
    for ducker_first in [true, false] {
        let mut s = ducked_show(ducker_first, 5.0, -40.0, 1.0);
        s.run(u64::from(RATE) * 2);
        let p = s.now() as usize;
        s.transport(TransportAction::Panic);
        s.run(u64::from(RATE) * 2);
        let out = s.channel(0);
        assert_eq!(rise_after(&out, p), 0.0, "ducker first {ducker_first}");
    }
}

/// The largest step between two samples from `from` on.
fn largest_step(out: &[f32], from: usize) -> f32 {
    out[from..]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max)
}

/// A duck coming off (its cue ended at 3 s, a 2 s return) when, 1.5 s into the return, a Panic, a
/// stop with a fade or fade-and-stop-others starts: the return stops where it is and the sound
/// only goes down from there.
#[test]
fn a_duck_return_under_way_stops_rising_when_the_cue_starts_fading_out() {
    let stop_fade = TransportAction::Stop {
        cue: id(1),
        fade_out: Some(openlab_core::FadeSpec {
            duration: 1.0,
            curve: FadeCurve::Linear,
        }),
    };
    let late = u64::from(RATE) * 45 / 10;
    for (what, action) in [
        ("panic", TransportAction::Panic),
        ("stop with a fade", stop_fade),
    ] {
        let mut s = ducked_show(false, 3.0, -40.0, 2.0);
        s.run(late);
        let p = s.now() as usize;
        s.transport(action);
        s.run(u64::from(RATE) * 2);
        let out = s.channel(0);
        // The hold is a de-zipper to where it is: a hair, never the return's rise.
        assert!(rise_after(&out, p + 64) == 0.0, "{what}");
        assert!(
            out[p + 64] <= out[p] * 1.01,
            "{what}: {} then {}",
            out[p],
            out[p + 64]
        );
    }
    let path = wav(&dc(RATE as usize * 10, LEVEL));
    let mut ducker = cue(2, CueKind::Wait { duration: 3.0 });
    ducker.duck_others = openlab_core::DuckOthers {
        enabled: true,
        db: -40.0,
        duration: 2.0,
    };
    let mut stopper = cue(3, CueKind::Memo);
    stopper.fade_and_stop_others = openlab_core::FadeAndStopOthers {
        enabled: true,
        scope: openlab_core::OthersScope::Peers,
        duration: 1.0,
    };
    let mut s = show(workspace(vec![audio(1, &path, 0), ducker, stopper]));
    start(&mut s, 1);
    start(&mut s, 2);
    s.run(late);
    let p = start(&mut s, 3) as usize;
    s.run(u64::from(RATE) * 2);
    let out = s.channel(0);
    assert!(rise_after(&out, p + 64) == 0.0, "fade and stop others");
    assert!(out[p + 64] <= out[p] * 1.01);
}

/// A +12 dB boost with the workspace maximum at +12: whatever raises the boosted cue's master (an
/// edit, a Fade cue) or moves the maximum (down or up), no rendered sample is over the maximum
/// and nothing steps: no sample-to-sample move bigger than the change's own ramp makes (the
/// master's ramp at the boost's height for an edit or a Fade; a 10 ms ramp over the whole range
/// for a moved maximum).
#[test]
fn a_boost_never_takes_a_rendered_sample_past_the_workspace_maximum_nor_steps() {
    let make = |master: f64, max: f64| {
        let path = wav(&dc(RATE as usize * 8, 0.1));
        let mut a = audio(1, &path, 0);
        if let CueKind::Audio { levels, .. } = &mut a.kind {
            levels.master_db = master;
        }
        let mut booster = cue(2, CueKind::Wait { duration: 6.0 });
        booster.duck_others = openlab_core::DuckOthers {
            enabled: true,
            db: 12.0,
            duration: 0.05,
        };
        let mut fade = cue(3, CueKind::Memo);
        fade.kind = CueKind::Fade {
            target: Some(id(1)),
            duration: 0.2,
            curve: FadeCurve::Linear,
            levels: FadeLevels {
                master_db: Some(12.0),
                output_db: vec![],
                crosspoints: vec![],
            },
            stop_target_when_done: false,
            mode: openlab_core::FadeMode::Absolute,
        };
        let mut ws = workspace(vec![a, booster, fade]);
        ws.settings.output_max_db = max;
        let mut s = show(ws);
        start(&mut s, 1);
        start(&mut s, 2);
        s.run(u64::from(RATE) / 2);
        s
    };
    let set_max = |s: &mut OfflineShow, value: f64| {
        let rid = s.request_id();
        let at = s.now() as usize;
        s.send(ClientMessage::Command {
            id: rid,
            command: Box::new(Command::SetWorkspaceSetting {
                setting: openlab_core::WorkspaceSetting::OutputMaxDb { value },
            }),
            coalesce: None,
        });
        at
    };
    // `ramp`: the largest step the change itself makes (a master ramp under the boost, or a
    // 10 ms ramp across the whole range); nothing steps beyond it.
    let check = |s: &mut OfflineShow, at: usize, max: f64, ramp: f32, what: &str| {
        s.run(1600);
        let out = s.channel(0);
        let cap = 0.1 * db_to_gain(max) * 1.0001;
        let peak = out[at - 10..].iter().copied().fold(0.0f32, f32::max);
        assert!(peak <= cap, "{what}: {peak} over {cap}");
        let step = largest_step(&out, at - 10);
        assert!(
            step <= ramp * 1.01,
            "{what}: a step of {step} (its ramp: {ramp})"
        );
    };
    let mut s = make(-12.0, 12.0);
    let boost = db_to_gain(12.0);
    let master_moves = 0.1 * (db_to_gain(12.0) - db_to_gain(-12.0)) * boost;
    let at = set_levels(&mut s, 1, |l| l.master_db = 12.0);
    check(&mut s, at, 12.0, master_moves / 80.0, "a level edit");
    let mut s = make(-12.0, 12.0);
    let at = start(&mut s, 3) as usize;
    check(&mut s, at, 12.0, master_moves / 1600.0, "a Fade cue");
    // The maximum lowered to 0 dB under a boosted cue at 0 dB: within the edit ramp nothing is
    // over the new maximum; before, nothing over the old.
    let mut s = make(0.0, 12.0);
    let at = set_max(&mut s, 0.0);
    s.run(1600);
    let out = s.channel(0);
    let low = 0.1 * db_to_gain(0.0) * 1.0001;
    let peak = out[at + 100..].iter().copied().fold(0.0f32, f32::max);
    assert!(peak <= low, "lowered maximum: {peak} over {low}");
    // The boost comes off over the 10 ms edit ramp, evenly in dB: its first step down is its
    // biggest, 12 dB / 80 samples of the boosted level. Before this was fixed it was all of
    // 12 dB in one sample.
    let even_db_step = 0.1 * boost * (1.0 - 10f32.powf(-12.0 / 20.0 / 80.0));
    let step = largest_step(&out, at - 10);
    assert!(
        step <= even_db_step * 1.01,
        "lowered maximum: a step of {step} (its ramp: {even_db_step})"
    );
    // Raised from +6: the boost comes up to the new room, smoothly.
    let mut s = make(0.0, 6.0);
    let at = set_max(&mut s, 12.0);
    check(&mut s, at, 12.0, 0.1 * boost / 80.0, "a raised maximum");
    let out = s.channel(0);
    assert!(
        out[out.len() - 1] > 0.1 * db_to_gain(11.0),
        "the boost came up"
    );
}

/// The duck moves evenly in dB: -12 dB over 1 s is at -3, -6 and -9 dB a quarter, half and three
/// quarters of the way, and its return the same backwards; a duck to silence and its return, and
/// a duck changed half way (a second ducking cue), make no step.
#[test]
fn a_duck_moves_evenly_in_db_and_never_steps() {
    let rate = RATE as usize;
    let db = |x: f32| 20.0 * (x / LEVEL).log10();
    let mut s = ducked_show(false, 2.0, -12.0, 1.0);
    let at = s.now() as usize;
    s.run(u64::from(RATE) * 4);
    let out = s.channel(0);
    for (k, want) in [(1, -3.0), (2, -6.0), (3, -9.0)] {
        let got = db(out[at + k * rate / 4]);
        assert!((got - want).abs() < 0.5, "down, {k}/4: {got} dB");
        // The return starts when the Wait ends, 2 s after the duck did.
        let got = db(out[at + 2 * rate + k * rate / 4]);
        assert!((got - (-12.0 - want)).abs() < 0.5, "back, {k}/4: {got} dB");
    }
    // To silence and back.
    let mut s = ducked_show(false, 2.0, -120.0, 1.0);
    let at = s.now() as usize;
    s.run(u64::from(RATE) * 4);
    let out = s.channel(0);
    assert!(
        out[at + rate + 10] < 1e-5,
        "silent: {}",
        out[at + rate + 10]
    );
    // From the sample after the start (the start itself is the file's first sample).
    let step = largest_step(&out, at + 1);
    assert!(step < LEVEL * 0.003, "a step of {step}");
    // A second duck half way through the first: the stage goes on from where it is.
    let path = wav(&dc(rate * 6, LEVEL));
    let ducker = |n, db| {
        let mut c = cue(n, CueKind::Wait { duration: 3.0 });
        c.duck_others = openlab_core::DuckOthers {
            enabled: true,
            db,
            duration: 1.0,
        };
        c
    };
    let mut s = show(workspace(vec![
        audio(1, &path, 0),
        ducker(2, -12.0),
        ducker(3, -12.0),
    ]));
    start(&mut s, 1);
    let at = start(&mut s, 2) as usize;
    s.run(u64::from(RATE) / 2);
    start(&mut s, 3);
    s.run(u64::from(RATE) * 4);
    let out = s.channel(0);
    // From the sample after the start (the start itself is the file's first sample).
    let step = largest_step(&out, at + 1);
    assert!(step < LEVEL * 0.002, "a step of {step}");
    assert!(
        (db(out[at + 2 * rate]) + 24.0).abs() < 0.5,
        "both ducks: {}",
        db(out[at + 2 * rate])
    );
}
