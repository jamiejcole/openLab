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
//! what is heard. Rules: docs/cue-engine.md, "Level edits on a playing cue".

use crate::show::*;
use openlab_core::protocol::{ClientMessage, Outcome, ServerMessage, TransportAction};
use openlab_core::{
    Command, Crosspoint, CueKind, CueLevels, CueProperty, FadeCurve, FadeLevels, GroupMode,
    OutputPatch, PatchId, PatchMapping, PatchOutput, db_to_gain,
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
        },
    )
}

#[test]
fn a_running_fade_keeps_the_master_it_moves_but_a_mute_is_heard() {
    // The same fade twice; in the second show the operator edits the master mid-fade.
    let mut plain = long_show(vec![master_fade(2, 1, 800, -20.0)]);
    let mut edited = long_show(vec![master_fade(2, 1, 800, -20.0)]);
    for s in [&mut plain, &mut edited] {
        start(s, 1);
        s.run(800);
        start(s, 2);
        s.run(400);
    }
    set_levels(&mut edited, 1, |l| l.master_db = -6.0);
    plain.run(800);
    edited.run(800);
    // The fade wins: the edit is in the model, the room hears the fade as programmed.
    assert_eq!(plain.channel(0), edited.channel(0));

    // A mute is heard even on a level a fade is moving.
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
                delay_ms: 0.0,
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
