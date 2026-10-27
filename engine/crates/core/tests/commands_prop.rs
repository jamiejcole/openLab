// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Property tests for editing commands: random command sequences against random workspaces.
//! Every command round-trips through undo and redo, a refused command changes nothing, and
//! nothing a command does can make `validate` report a value problem or `save_show` refuse.

mod common;

use common::*;
use openlab_core::*;
use proptest::prelude::*;

/// Random numbers for one command; [`build`] turns them into a command that fits the workspace
/// as it is. Some come out invalid on purpose.
#[derive(Clone, Debug)]
struct Pick {
    op: u8,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
}

fn pick() -> impl Strategy<Value = Pick> {
    (
        0u8..OPS,
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(|(op, a, b, c, d)| Pick { op, a, b, c, d })
}

fn picks(max: usize) -> impl Strategy<Value = Vec<Pick>> {
    proptest::collection::vec(pick(), 0..max)
}

/// Case count of a property: `default`, or `OPENLAB_PROP_CASES` for a long sweep, e.g.
/// `OPENLAB_PROP_CASES=5000 cargo test --release -p openlab-core --test commands_prop`.
fn cases(default: u32) -> u32 {
    std::env::var("OPENLAB_PROP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Operations 0-10 edit the cue tree; 11 and up edit settings, patches and cue lists.
const OPS: u8 = 24;

struct Gen {
    next: u128,
}

impl Gen {
    fn fresh(&mut self) -> CueId {
        self.next += 1;
        cue_id(self.next)
    }
}

fn choose<T: Copy>(items: &[T], n: u32) -> Option<T> {
    items.get(n as usize % items.len().max(1)).copied()
}

fn all_ids(ws: &Workspace) -> Vec<CueId> {
    fn walk(cues: &[Cue], out: &mut Vec<CueId>) {
        for cue in cues {
            out.push(cue.id);
            walk(cue.children(), out);
        }
    }
    let mut out = Vec::new();
    for list in &ws.cue_lists {
        walk(&list.cues, &mut out);
    }
    out
}

/// The groups of one list.
fn group_ids(ws: &Workspace, list: usize) -> Vec<CueId> {
    fn walk(cues: &[Cue], out: &mut Vec<CueId>) {
        for cue in cues {
            if matches!(cue.kind, CueKind::Group { .. }) {
                out.push(cue.id);
            }
            walk(cue.children(), out);
        }
    }
    let mut out = Vec::new();
    walk(&ws.cue_lists[list].cues, &mut out);
    out
}

fn children_len(ws: &Workspace, list: usize, parent: Option<CueId>) -> usize {
    match parent {
        None => ws.cue_lists[list].cues.len(),
        Some(id) => ws.find_cue(id).map_or(0, |c| c.children().len()),
    }
}

/// How many cues share a container with `id` (its group, or its list).
fn siblings_of(ws: &Workspace, id: CueId) -> Option<usize> {
    fn search(cues: &[Cue], id: CueId) -> Option<usize> {
        if cues.iter().any(|c| c.id == id) {
            return Some(cues.len());
        }
        cues.iter().find_map(|c| search(c.children(), id))
    }
    ws.cue_lists.iter().find_map(|l| search(&l.cues, id))
}

/// A position in either list that is sometimes one or two past the end, and so refused.
fn position(ws: &Workspace, a: u32, b: u32) -> Position {
    let list = a as usize / 2 % ws.cue_lists.len();
    let mut parents = vec![None];
    parents.extend(group_ids(ws, list).into_iter().map(Some));
    let parent = choose(&parents, a).flatten();
    Position {
        list: ws.cue_lists[list].id,
        parent,
        index: b as usize % (children_len(ws, list, parent) + 2),
    }
}

/// Mostly valid; NaN, infinity, negatives and too-large values now and then.
fn time(n: u32) -> f64 {
    match n % 14 {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1.0,
        3 => 1e9,
        k => [0.0, 0.5, 1.0, 2.5, 30.0, 100.0][k as usize % 6],
    }
}

fn level(n: u32) -> f64 {
    match n % 14 {
        0 => f64::NAN,
        1 => 20.0,
        2 => f64::NEG_INFINITY,
        3 => -200.0,
        k => [0.0, -6.0, -12.0, -120.0, 6.0, 12.0][k as usize % 6],
    }
}

fn number(n: u32) -> Option<String> {
    match n % 12 {
        0..=4 => None,
        11 => Some(String::new()),
        k => Some(k.to_string()),
    }
}

/// Any kind but a Group.
fn kind(ws: &Workspace, p: &Pick) -> CueKind {
    let ids = all_ids(ws);
    match p.a % 7 {
        0 => CueKind::Memo,
        1 => CueKind::Wait {
            duration: time(p.b),
        },
        2 | 3 => {
            let mut kind = audio();
            if let CueKind::Audio {
                levels,
                start_time,
                media,
                patch,
                preload,
                ..
            } = &mut kind
            {
                levels.master_db = level(p.b);
                levels.outputs = vec![out(level(p.c)); (p.d % 4) as usize];
                *start_time = time(p.d / 4);
                // A few files, two patches and some preloads, so the media and patch
                // bookkeeping has something to tell apart.
                *media =
                    (!p.c.is_multiple_of(13)).then(|| MediaRef::new(MEDIA[(p.b / 5) as usize % 4]));
                *patch = patch_id(1 + u128::from(p.c / 3 % 2));
                *preload = (p.c / 7).is_multiple_of(2);
            }
            kind
        }
        4 => {
            let target = choose(&ids, p.b);
            CueKind::Fade {
                target,
                duration: time(p.c),
                curve: FadeCurve::Linear,
                levels: FadeLevels {
                    master_db: Some(level(p.d)),
                    output_db: vec![],
                    crosspoints: vec![],
                },
                stop_target_when_done: false,
            }
        }
        5 => CueKind::Arm {
            target: choose(&ids, p.b),
        },
        _ => CueKind::Network {
            destination: net_id(1),
            address: "/x".into(),
            args: vec![OscArg::Float {
                value: p.b as f32 / 7.0,
            }],
        },
    }
}

fn make_cue(ws: &Workspace, g: &mut Gen, p: &Pick) -> Cue {
    let id = g.fresh();
    let mut cue = if p.a % 11 == 10 {
        // A group holding a memo and another group.
        let inner = common::cue(
            g.fresh().0.as_u128(),
            group(vec![common::cue(g.fresh().0.as_u128(), CueKind::Memo)]),
        );
        let child = common::cue(g.fresh().0.as_u128(), CueKind::Memo);
        common::cue(id.0.as_u128(), group(vec![child, inner]))
    } else {
        common::cue(id.0.as_u128(), kind(ws, p))
    };
    cue.number = number(p.c);
    cue.name = format!("cue {}", id.0.as_u128());
    cue
}

fn property(ws: &Workspace, variant: u8, p: &Pick) -> CueProperty {
    match variant % 14 {
        0 => CueProperty::Name {
            value: format!("name {}", p.b),
        },
        1 => CueProperty::Notes {
            value: format!("notes {}", p.b),
        },
        2 => CueProperty::Color {
            value: [
                CueColor::None,
                CueColor::Red,
                CueColor::Orange,
                CueColor::Green,
                CueColor::Blue,
                CueColor::Purple,
            ][p.b as usize % 6],
        },
        3 => CueProperty::Flagged {
            value: p.b.is_multiple_of(2),
        },
        4 => CueProperty::Armed {
            value: p.b.is_multiple_of(2),
        },
        5 => CueProperty::PreWait { value: time(p.b) },
        6 => CueProperty::PostWait { value: time(p.b) },
        7 => CueProperty::ContinueMode {
            value: [
                ContinueMode::DoNotContinue,
                ContinueMode::AutoContinue,
                ContinueMode::AutoFollow,
            ][p.b as usize % 3],
        },
        8 => CueProperty::Triggers {
            value: Triggers {
                hotkey: ["a", "f5", "Bad Key", ""]
                    .get(p.b as usize % 5)
                    .map(|key| Hotkey {
                        key: (*key).into(),
                        modifiers: Modifiers::default(),
                    }),
                osc: p.c.is_multiple_of(2).then(|| "/go".into()),
            },
        },
        9 => CueProperty::Target {
            value: choose(&all_ids(ws), p.b),
        },
        10 => CueProperty::GroupMode {
            value: [
                GroupMode::Timeline,
                GroupMode::StartRandom,
                GroupMode::Playlist {
                    loop_: true,
                    shuffle: false,
                },
            ][p.b as usize % 3],
        },
        12 => CueProperty::SecondTrigger {
            value: [
                SecondTrigger::Nothing,
                SecondTrigger::Panic,
                SecondTrigger::Stop,
                SecondTrigger::HardStop,
                SecondTrigger::HardStopAndRestart,
                SecondTrigger::Devamp,
                SecondTrigger::PlayNext,
                SecondTrigger::PlayPrevious,
            ][p.b as usize % 8],
        },
        13 => CueProperty::SecondTriggerOnRelease {
            value: p.b.is_multiple_of(2),
        },
        _ => CueProperty::Kind { value: kind(ws, p) },
    }
}

/// Every id under `cue` (itself first), in order.
fn subtree(cue: &Cue) -> Vec<CueId> {
    let mut out = vec![cue.id];
    for c in cue.children() {
        out.extend(subtree(c));
    }
    out
}

/// `cue` and every cue below it.
fn flat(cue: &Cue) -> Vec<&Cue> {
    let mut out = vec![cue];
    for c in cue.children() {
        out.extend(flat(c));
    }
    out
}

/// How deep the groups below `cue` nest (a plain cue is 0, a group of plain cues 1).
fn depth(cue: &Cue) -> usize {
    cue.children()
        .iter()
        .map(|c| 1 + depth(c))
        .max()
        .unwrap_or(0)
}

/// A copy of `cue` with the same ids and other content: some descendants get another kind,
/// another descendant cue under a group's id, or a group of their own.
fn rewrite(cue: &Cue, ws: &Workspace, g: &mut Gen, p: &Pick, salt: u32) -> Cue {
    let mut out = cue.clone();
    if let CueKind::Group { children, .. } = &mut out.kind {
        for (i, child) in children.iter_mut().enumerate() {
            let pick =
                p.a.wrapping_add(salt.wrapping_mul(7))
                    .wrapping_add(i as u32 * 3)
                    % 5;
            let rewritten = rewrite(child, ws, g, p, salt + 1);
            *child = match (pick, &rewritten.kind) {
                (0, _) => rewritten,
                (1 | 2, CueKind::Group { .. }) => rewritten,
                (1, _) => common::cue(rewritten.id.0.as_u128(), kind(ws, p)),
                // The same id, now a group around a new cue.
                (3, _) => common::cue(
                    rewritten.id.0.as_u128(),
                    group(vec![common::cue(g.fresh().0.as_u128(), kind(ws, p))]),
                ),
                // A fresh cue where it was, the old id gone.
                _ => common::cue(g.fresh().0.as_u128(), kind(ws, p)),
            };
        }
    }
    out
}

/// A transaction that removes a cue and adds back a cue with its id (anywhere): a group with
/// other children, a plain cue, a copy of the removed cue (what undoing a removal does), a
/// nested group that reuses the old descendants' ids with other content, or the old subtree
/// rewritten in place (same ids, changed kinds). The index, the problem bookkeeping and the
/// media catalog must treat the whole subtree as new.
fn readd(ws: &Workspace, g: &mut Gen, p: &Pick) -> Option<Vec<Command>> {
    let ids = all_ids(ws);
    // Half of the time a group (a nested one when there is one), where the shapes are.
    let groups: Vec<CueId> = ids
        .iter()
        .copied()
        .filter(|&i| ws.find_cue(i).is_some_and(|c| depth(c) >= 1))
        .collect();
    let deep: Vec<CueId> = groups
        .iter()
        .copied()
        .filter(|&i| ws.find_cue(i).is_some_and(|c| depth(c) >= 2))
        .collect();
    let id = match p.b % 5 {
        3 | 4 => choose(&deep, p.a).or_else(|| choose(&groups, p.a)),
        _ => None,
    }
    .or_else(|| choose(&ids, p.a))?;
    let old = ws.find_cue(id)?;
    let mut evolving = ws.clone();
    let remove = Command::RemoveCue { id };
    evolving.apply(remove.clone()).ok()?;
    let reuse = subtree(old);
    let new = match p.b % 5 {
        0 => {
            let kids = (0..p.c % 3)
                .map(|_| common::cue(g.fresh().0.as_u128(), kind(&evolving, p)))
                .collect();
            common::cue(id.0.as_u128(), group(kids))
        }
        1 => common::cue(id.0.as_u128(), kind(&evolving, p)),
        2 => old.clone(),
        3 => {
            // group id { group (old id) { leaf (old id), leaf } }: two levels, ids kept.
            let mut pick = |n: usize| reuse.get(n).copied().unwrap_or_else(|| g.fresh());
            let (inner, leaf) = (pick(1), pick(2));
            let leaf = common::cue(leaf.0.as_u128(), kind(&evolving, p));
            let other = common::cue(g.fresh().0.as_u128(), kind(&evolving, p));
            common::cue(
                id.0.as_u128(),
                group(vec![common::cue(
                    inner.0.as_u128(),
                    group(vec![leaf, other]),
                )]),
            )
        }
        _ => rewrite(old, &evolving, g, p, 0),
    };
    let add = Command::AddCue {
        at: position(&evolving, p.c, p.d),
        cue: new,
        auto_number: false,
    };
    evolving.apply(add.clone()).ok()?;
    Some(vec![remove, add])
}

fn setting_value(p: &Pick) -> WorkspaceSetting {
    let key = [
        TransportKey::Go,
        TransportKey::StopAll,
        TransportKey::PauseAll,
        TransportKey::ResumeAll,
        TransportKey::PlayheadNext,
        TransportKey::PlayheadPrevious,
    ][p.c as usize % 6];
    match p.b % 9 {
        0 => WorkspaceSetting::MinGoInterval {
            value: time(p.c) / 10.0,
        },
        1 => WorkspaceSetting::PanicDuration { value: time(p.c) },
        2 => WorkspaceSetting::AutosaveInterval {
            value: time(p.c) * 10.0,
        },
        3 => WorkspaceSetting::OscPasscode {
            value: [
                None,
                Some("pw".to_owned()),
                Some(String::new()),
                Some("x".repeat(300)),
            ][p.c as usize % 4]
                .clone(),
        },
        4 => WorkspaceSetting::MasterDb { value: level(p.c) },
        5 | 6 => WorkspaceSetting::Hotkey {
            key,
            value: ["a", "f5", "Bad Key", ""]
                .get(p.d as usize % 5)
                .map(|k| Hotkey {
                    key: (*k).into(),
                    modifiers: Modifiers::default(),
                }),
        },
        7 => WorkspaceSetting::AutoNumberStart { value: time(p.c) },
        _ => WorkspaceSetting::AutoNumberIncrement { value: time(p.c) },
    }
}

fn patch_output_from(p: &Pick) -> PatchOutput {
    PatchOutput {
        level_db: level(p.c),
        mute: p.d.is_multiple_of(2),
        delay_ms: time(p.d) * 10.0,
        polarity_inverted: p.b.is_multiple_of(2),
    }
}

fn patch_property(p: &Pick) -> PatchProperty {
    let output = p.c as usize % 4;
    match p.b % 8 {
        0 => PatchProperty::Name {
            value: format!("patch {}", p.d),
        },
        1 => PatchProperty::MasterDb { value: level(p.d) },
        2 => PatchProperty::OutputLevel {
            output,
            value: level(p.d),
        },
        3 => PatchProperty::OutputMute {
            output,
            value: p.d.is_multiple_of(2),
        },
        4 => PatchProperty::OutputDelay {
            output,
            value: time(p.d) * 10.0,
        },
        5 => PatchProperty::OutputPolarity {
            output,
            value: p.d.is_multiple_of(2),
        },
        6 => PatchProperty::ChannelCount {
            value: [1, 2, 3, 8, 0, 65, usize::MAX][p.d as usize % 7],
        },
        _ => PatchProperty::Outputs {
            value: vec![patch_output_from(p); [1, 2, 0, 65][p.d as usize % 4]],
        },
    }
}

fn network_property(p: &Pick) -> NetworkPatchProperty {
    match p.b % 4 {
        0 => NetworkPatchProperty::Name {
            value: format!("net {}", p.d),
        },
        1 => NetworkPatchProperty::Host {
            value: format!("10.0.0.{}", p.d % 255),
        },
        2 => NetworkPatchProperty::Port {
            value: (p.d % 65536) as u16,
        },
        _ => NetworkPatchProperty::Protocol {
            value: if p.d.is_multiple_of(2) {
                NetworkProtocol::Udp
            } else {
                NetworkProtocol::Tcp
            },
        },
    }
}

/// Settings, patches, network patches and cue lists: operations 11 to 23. Ids are drawn from a
/// small pool, so they collide with existing ones (refused) and re-use removed ones.
fn build_model(ws: &Workspace, op: u8, p: &Pick) -> Option<Command> {
    let patch_ids: Vec<PatchId> = ws.patches.iter().map(|x| x.id).collect();
    let net_ids: Vec<NetworkPatchId> = ws.network_patches.iter().map(|x| x.id).collect();
    let list_ids: Vec<CueListId> = ws.cue_lists.iter().map(|x| x.id).collect();
    // An existing id most of the time, a made-up one now and then.
    let some_patch = || {
        choose(&patch_ids, p.a)
            .filter(|_| p.a % 9 != 8)
            .unwrap_or(patch_id(9))
    };
    let some_net = || {
        choose(&net_ids, p.a)
            .filter(|_| p.a % 9 != 8)
            .unwrap_or(net_id(9))
    };
    let some_list = || {
        choose(&list_ids, p.a)
            .filter(|_| p.a % 9 != 8)
            .unwrap_or(list_id(9))
    };
    Some(match op {
        11 => Command::SetWorkspaceSetting {
            setting: setting_value(p),
        },
        12 => {
            let mut new = common::patch(
                1 + u128::from(p.c % 5),
                [1, 2, 4, 8, 0, 65][p.d as usize % 6],
            );
            new.master_db = level(p.a / 16);
            Command::AddPatch {
                index: p.b as usize % (ws.patches.len() + 2),
                patch: new,
            }
        }
        13 => Command::RemovePatch {
            id: some_patch(),
            even_if_used: p.c.is_multiple_of(5),
        },
        14 => Command::MovePatch {
            id: some_patch(),
            index: p.b as usize % (ws.patches.len() + 1),
        },
        15 => Command::SetPatchProperty {
            id: some_patch(),
            property: patch_property(p),
        },
        16 => Command::AddNetworkPatch {
            index: p.b as usize % (ws.network_patches.len() + 2),
            patch: NetworkPatch {
                id: net_id(1 + u128::from(p.c % 4)),
                name: format!("n{}", p.d),
                host: "h".into(),
                port: 1,
                protocol: NetworkProtocol::Udp,
            },
        },
        17 => Command::RemoveNetworkPatch {
            id: some_net(),
            even_if_used: p.c.is_multiple_of(5),
        },
        18 => Command::MoveNetworkPatch {
            id: some_net(),
            index: p.b as usize % (ws.network_patches.len() + 1),
        },
        19 => Command::SetNetworkPatchProperty {
            id: some_net(),
            property: network_property(p),
        },
        20 => Command::AddCueList {
            index: p.b as usize % (ws.cue_lists.len() + 2),
            id: list_id(1 + u128::from(p.c % 4)),
            name: format!("list {}", p.d),
        },
        21 => Command::RemoveCueList { id: some_list() },
        22 => Command::MoveCueList {
            id: some_list(),
            index: p.b as usize % (ws.cue_lists.len() + 1),
        },
        23 => Command::RenameCueList {
            id: some_list(),
            name: format!("list {}", p.d % 4),
        },
        _ => return None,
    })
}

/// Operations 0–10 are the cue commands, 11–23 the model commands (`build_model`); 100 + n is
/// `SetCueProperty` with property variant n.
fn build(ws: &Workspace, g: &mut Gen, op: u8, p: &Pick) -> Option<Command> {
    let ids = all_ids(ws);
    let id = choose(&ids, p.a);
    Some(match op {
        0 => Command::AddCue {
            at: position(ws, p.b, p.c),
            cue: make_cue(ws, g, p),
            auto_number: false,
        },
        1 => Command::AddCue {
            at: position(ws, p.b, p.c),
            cue: make_cue(ws, g, p),
            auto_number: true,
        },
        2 => {
            let clipboard: Vec<Cue> = [choose(&ids, p.a)?, choose(&ids, p.b)?]
                .iter()
                .take(1 + (p.c % 2) as usize)
                .map(|&id| ws.find_cue(id).unwrap().clone())
                .collect();
            // Copying a cue together with its own descendant would paste it twice: fine, ids
            // are fresh each time.
            let cues = prepare_paste(ws, &clipboard, || g.fresh()).ok()?;
            Command::Paste {
                at: position(ws, p.c, p.d),
                cues,
            }
        }
        3 => Command::RemoveCue { id: id? },
        4 => {
            let id = id?;
            let siblings = siblings_of(ws, id)?;
            Command::MoveCue {
                id,
                index: p.b as usize % (siblings + 1),
            }
        }
        5 => Command::ReparentCue {
            id: id?,
            to: position(ws, p.b, p.c),
        },
        6 => Command::SetCueProperty {
            id: id?,
            property: property(ws, (p.d % 14) as u8, p),
        },
        7 => {
            let selected = (0..1 + p.c % 3).filter_map(|n| choose(&ids, p.a.wrapping_add(n * 7)));
            Command::BatchSet {
                ids: selected.collect(),
                // Every variant but `Kind` (11).
                property: property(
                    ws,
                    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13][p.d as usize % 13],
                    p,
                ),
            }
        }
        8 => Command::SetCueNumbers {
            numbers: (0..1 + p.c % 3)
                .filter_map(|n| {
                    Some(NumberAssignment {
                        id: choose(&ids, p.a.wrapping_add(n * 5))?,
                        number: number(p.b.wrapping_add(n * 3)),
                    })
                })
                .collect(),
        },
        9 => Command::Renumber {
            ids: (0..1 + p.c % 4)
                .filter_map(|n| choose(&ids, p.a.wrapping_add(n * 3)))
                .collect(),
            start: [1.0, 0.0, 10.0, 0.1, -1.0, f64::NAN][p.b as usize % 6],
            increment: [1.0, 5.0, 0.1, 0.0, f64::INFINITY, 0.5][p.d as usize % 6],
        },
        10 => {
            if p.d % 4 == 3
                && let Some(commands) = readd(ws, g, p)
            {
                return Some(Command::Transaction { commands });
            }
            // Each inner command is built against the workspace the earlier ones leave, so a
            // cue added in the transaction can be edited, moved or removed later in it. Inner
            // commands that would be refused are left out (the whole transaction would be).
            let mut evolving = ws.clone();
            let mut commands = Vec::new();
            let mut added: Option<CueId> = None;
            for n in 0..2 + p.d % 2 {
                let mut inner = Pick {
                    a: p.a.wrapping_add(n * 11),
                    b: p.b.wrapping_add(n * 13),
                    ..p.clone()
                };
                let mut op = (p.c as u8).wrapping_add(n as u8) % 10;
                // Half the time, act on the cue the transaction just added: remove, move,
                // reparent, edit or batch-edit it.
                if let Some(id) = added.filter(|_| (p.d / 3).is_multiple_of(2)) {
                    let at = all_ids(&evolving).iter().position(|&i| i == id);
                    inner.a = at.unwrap_or(0) as u32;
                    op = [3, 4, 5, 6, 6, 7][(p.c / 7) as usize % 6];
                }
                let Some(command) = build(&evolving, g, op, &inner) else {
                    continue;
                };
                if evolving.apply(command.clone()).is_ok() {
                    if let Some(&id) = command.footprint().added.first() {
                        added = Some(id);
                    }
                    commands.push(command);
                }
            }
            Command::Transaction { commands }
        }
        100..=113 => Command::SetCueProperty {
            id: id?,
            property: property(ws, op - 100, p),
        },
        _ => return build_model(ws, op, p),
    })
}

const MEDIA: [&str; 4] = ["media/a.wav", "media/b.wav", "media/c.wav", "media/d.wav"];

fn base() -> Workspace {
    let mut ws = workspace(vec![]);
    ws.patches.push(patch(2, 2));
    // A second list, so positions, moves between lists and mirrors of both are exercised.
    ws.cue_lists.push(CueList {
        id: list_id(2),
        name: "Second".into(),
        cues: vec![],
    });
    ws.settings.auto_number_start = 1.0;
    ws.settings.auto_number_increment = 1.0;
    ws
}

/// A workspace and id generator after the random `prefix` of commands (refused ones skipped).
fn after_prefix(prefix: &[Pick]) -> (Workspace, Gen) {
    let (mut ws, mut g) = (base(), Gen { next: 1000 });
    for p in prefix {
        if let Some(command) = build(&ws, &mut g, p.op, p) {
            let _ = ws.apply(command);
        }
    }
    (ws, g)
}

fn bytes(ws: &Workspace) -> String {
    serde_json::to_string(ws).unwrap()
}

/// What commands may leave behind: broken references, silent extra outputs and playlist-only
/// second triggers on other cues, never a value
/// problem or an error. And the show saves and loads back identical.
fn check_invariants(ws: &Workspace) -> Result<(), TestCaseError> {
    for issue in validate(ws) {
        prop_assert!(
            matches!(
                issue.kind,
                IssueKind::TargetMissing { .. }
                    | IssueKind::TargetWrongKind { .. }
                    | IssueKind::TargetSelf
                    | IssueKind::UnpatchedOutputs { .. }
                    // Kept and inert, like a wrong-kind target (a group-mode change makes it
                    // apply again).
                    | IssueKind::SecondTriggerNeedsPlaylist
                    // A cue may name a patch the workspace lacks (it was removed with
                    // `even_if_used`, or the cue was added after): the broken-cue state.
                    | IssueKind::PatchMissing { .. }
                    | IssueKind::NetworkPatchMissing { .. }
                    // A transport key name is kept as given, like load does (it only warns).
                    | IssueKind::InvalidHotkey { .. }
            ) && issue.severity == Severity::Warning,
            "command introduced {issue}"
        );
    }
    let text = save_show(ws).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let loaded = load_show(&text).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(&loaded.workspace, ws);
    Ok(())
}

/// Applies `command` to a copy: refused means unchanged; applied means undo restores the
/// original and redo the result, and the invariants hold. Returns whether it applied.
fn check_command(ws: &Workspace, command: Command) -> Result<bool, TestCaseError> {
    let mut work = ws.clone();
    let Ok(inverse) = work.apply(command) else {
        prop_assert_eq!(&work, ws);
        prop_assert_eq!(bytes(&work), bytes(ws));
        return Ok(false);
    };
    let after = work.clone();
    check_invariants(&after)?;
    let redo = work
        .apply(inverse)
        .map_err(|e| TestCaseError::fail(format!("undo: {e}")))?;
    prop_assert_eq!(&work, ws);
    prop_assert_eq!(bytes(&work), bytes(ws));
    work.apply(redo)
        .map_err(|e| TestCaseError::fail(format!("redo: {e}")))?;
    prop_assert_eq!(&work, &after);
    Ok(true)
}

fn round_trip(prefix: &[Pick], op: u8, p: &Pick) -> Result<(), TestCaseError> {
    let (ws, mut g) = after_prefix(prefix);
    if let Some(command) = build(&ws, &mut g, op, p) {
        check_command(&ws, command)?;
    }
    Ok(())
}

macro_rules! round_trip_tests {
    ($($name:ident: $op:expr,)*) => {
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(cases(64)))]
            $(
                #[test]
                fn $name(prefix in picks(30), p in pick()) {
                    round_trip(&prefix, $op, &p)?;
                }
            )*
        }
    };
}

round_trip_tests! {
    add_cue_round_trips: 0,
    add_cue_with_auto_number_round_trips: 1,
    paste_round_trips: 2,
    remove_cue_round_trips: 3,
    move_cue_round_trips: 4,
    reparent_cue_round_trips: 5,
    set_property_round_trips: 6,
    batch_set_round_trips: 7,
    set_cue_numbers_round_trips: 8,
    renumber_round_trips: 9,
    transaction_round_trips: 10,
    set_name_round_trips: 100,
    set_notes_round_trips: 101,
    set_color_round_trips: 102,
    set_flagged_round_trips: 103,
    set_armed_round_trips: 104,
    set_pre_wait_round_trips: 105,
    set_post_wait_round_trips: 106,
    set_continue_mode_round_trips: 107,
    set_triggers_round_trips: 108,
    set_target_round_trips: 109,
    set_group_mode_round_trips: 110,
    set_kind_round_trips: 111,
    set_second_trigger_round_trips: 112,
    set_second_trigger_on_release_round_trips: 113,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(128)))]

    /// Whatever happens, every step keeps the invariants; undoing everything gives back the
    /// empty workspace step by step, and redoing everything the final one.
    #[test]
    fn random_sequences_undo_and_redo_exactly(sequence in picks(60)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        let mut states = vec![ws.clone()];
        for p in &sequence {
            let Some(command) = build(&ws, &mut g, p.op, p) else { continue };
            let before = ws.clone();
            let recorded = history.undo_len();
            if history.execute(&mut ws, command).is_err() {
                prop_assert_eq!(&ws, &before);
                prop_assert_eq!(bytes(&ws), bytes(&before));
                continue;
            }
            check_invariants(&ws)?;
            if history.undo_len() > recorded {
                states.push(ws.clone());
            }
        }
        let last = ws.clone();
        for expected in states.iter().rev().skip(1) {
            prop_assert!(history.undo(&mut ws).unwrap());
            prop_assert_eq!(&ws, expected);
        }
        prop_assert!(!history.undo(&mut ws).unwrap());
        for expected in states.iter().skip(1) {
            prop_assert!(history.redo(&mut ws).unwrap());
            prop_assert_eq!(&ws, expected);
        }
        prop_assert_eq!(&ws, &last);
    }

    /// Edits grouped by random coalesce keys undo to the start in however many steps the keys
    /// allow, and redo to the end, exactly; fewer steps than edits when keys repeat.
    #[test]
    fn coalesced_sequences_undo_and_redo_exactly(sequence in picks(60)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let start = ws.clone();
        let mut history = History::new();
        let mut applied = 0usize;
        for p in &sequence {
            let Some(command) = build(&ws, &mut g, p.op, p) else { continue };
            let key = [None, Some("a"), Some("a"), Some("b")][p.d as usize % 4];
            let before = ws.clone();
            match history.execute_coalescing(&mut ws, command, key) {
                Ok(changed) => {
                    check_invariants(&ws)?;
                    applied += usize::from(changed);
                    // An edit that is recorded may still cancel itself out (remove and re-add).
                    prop_assert!(changed || ws == before, "an edit was dropped as a no-op");
                }
                Err(_) => prop_assert_eq!(&ws, &before),
            }
        }
        prop_assert!(history.undo_len() <= applied);
        let last = ws.clone();
        while history.undo(&mut ws).unwrap() {}
        prop_assert_eq!(&ws, &start);
        prop_assert_eq!(bytes(&ws), bytes(&start));
        while history.redo(&mut ws).unwrap() {}
        prop_assert_eq!(&ws, &last);
    }

    /// A new edit after undo drops everything that could have been redone.
    #[test]
    fn a_new_edit_clears_redo_whatever_came_before(sequence in picks(40), undos in 1usize..6) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        for p in &sequence {
            if let Some(command) = build(&ws, &mut g, p.op, p) {
                let _ = history.execute(&mut ws, command);
            }
        }
        for _ in 0..undos {
            history.undo(&mut ws).unwrap();
        }
        let redoable = history.can_redo();
        let fresh = Command::AddCue {
            at: Position { list: ws.cue_lists[0].id, parent: None, index: 0 },
            cue: common::cue(999_999, CueKind::Memo),
            auto_number: false,
        };
        history.execute(&mut ws, fresh).unwrap();
        prop_assert!(!history.can_redo(), "redo survived an edit (had redo: {redoable})");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(256)))]

    /// A client that starts from the workspace and applies each edit's `model_changed` ends
    /// every step with exactly the engine's workspace: commands (groups moved and deleted with
    /// children, renumbering, transactions), undo and redo alike (ED-07, NFR-01).
    #[test]
    fn model_changes_keep_a_mirror_identical(sequence in picks(80)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut mirror = ws.clone();
        let mut history = History::new();
        for p in &sequence {
            let footprint = match p.d % 9 {
                0 => history.peek_undo().map(Command::footprint),
                1 => history.peek_redo().map(Command::footprint),
                _ => None,
            };
            let changed = match (p.d % 9, footprint) {
                (0, Some(fp)) => history.undo(&mut ws).unwrap().then_some(fp),
                (1, Some(fp)) => history.redo(&mut ws).unwrap().then_some(fp),
                (0 | 1, None) => None,
                _ => {
                    let Some(command) = build(&ws, &mut g, p.op, p) else { continue };
                    let fp = command.footprint();
                    // Random gesture keys, so undo and redo of merged steps are mirrored too.
                    let key = [None, Some("a"), Some("a"), Some("b")][(p.a / 3) as usize % 4];
                    history
                        .execute_coalescing(&mut ws, command, key)
                        .is_ok_and(|changed| changed)
                        .then_some(fp)
                }
            };
            let Some(fp) = changed else { continue };
            let (upserts, deletes) = model_changes(&ws, &fp);
            // O(changed): never more than a delete and an upsert per named cue, plus the
            // subtrees of groups that were named.
            let named = fp.added.len() + fp.removed.len() + fp.moved.len() + fp.edited.len();
            // Plus what it names of settings, lists and patches (all of a collection that was
            // reordered: the client sorts it by their indices).
            let part = |touched_len: usize, reordered: bool, all: usize| {
                if reordered || touched_len > 1 {
                    all + touched_len
                } else {
                    touched_len
                }
            };
            let extra = usize::from(fp.settings)
                + part(
                    fp.lists.changed.len() + fp.lists.removed.len(),
                    fp.lists.reordered,
                    ws.cue_lists.len(),
                )
                + part(
                    fp.patches.changed.len() + fp.patches.removed.len(),
                    fp.patches.reordered,
                    ws.patches.len(),
                )
                + part(
                    fp.network_patches.changed.len() + fp.network_patches.removed.len(),
                    fp.network_patches.reordered,
                    ws.network_patches.len(),
                );
            prop_assert!(upserts.len() <= named + extra);
            prop_assert!(deletes.len() <= named + extra);
            mirror.apply_changes(&upserts, &deletes).unwrap();
            prop_assert_eq!(&mirror, &ws);
        }
    }
}

/// One random edit (a command, or an undo or redo of the last) applied to `ws`; returns its
/// footprint when something changed. The same choice as the mirror test makes.
fn random_edit(
    ws: &mut Workspace,
    g: &mut Gen,
    history: &mut History,
    p: &Pick,
) -> Option<Footprint> {
    match p.d % 9 {
        0 => {
            let fp = history.peek_undo().map(Command::footprint)?;
            history.undo(ws).unwrap().then_some(fp)
        }
        1 => {
            let fp = history.peek_redo().map(Command::footprint)?;
            history.redo(ws).unwrap().then_some(fp)
        }
        _ => {
            let command = build(ws, g, p.op, p)?;
            let fp = command.footprint();
            history
                .execute(ws, command)
                .is_ok_and(|changed| changed)
                .then_some(fp)
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(256)))]

    /// The cue index follows every edit from its footprint alone and equals one built from
    /// scratch after each (commands, undo, redo, transactions that add, move and remove).
    #[test]
    fn the_cue_index_follows_footprints(sequence in picks(80)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        let mut index = CueIndex::build(&ws);
        for p in &sequence {
            let Some(fp) = random_edit(&mut ws, &mut g, &mut history, p) else { continue };
            let structural: std::collections::BTreeSet<CueId> = fp
                .added
                .iter()
                .chain(&fp.removed)
                .chain(&fp.moved)
                .copied()
                .collect();
            let touched = structural.iter().chain(&fp.edited).copied().collect();
            let change = index.reconcile(&ws, &touched, &structural, &fp.added);
            // Only an edit of the cue lists themselves may make the index start over.
            prop_assert!(matches!(change, IndexChange::Delta(_)) || !fp.lists.is_empty());
            prop_assert!(index.matches(&ws), "index differs from a rebuild");
        }
    }
}

fn status_input(ws: &Workspace, p: &Pick) -> Input {
    let ids = all_ids(ws);
    let list = ws.cue_lists[p.a as usize % ws.cue_lists.len()].id;
    match p.c % 12 {
        0 | 1 => Input::Start {
            cue: choose(&ids, p.a).unwrap_or(cue_id(1)),
        },
        2 => Input::Stop {
            cue: choose(&ids, p.a).unwrap_or(cue_id(1)),
            fade_out: None,
        },
        3 => Input::Go { list },
        4 | 5 => Input::MediaStatus {
            media: MediaRef::new(MEDIA[p.b as usize % 4]),
            status: [
                MediaStatus::Missing,
                MediaStatus::Available,
                MediaStatus::Unsupported {
                    reason: "no".into(),
                },
                MediaStatus::Unknown,
            ][p.d as usize % 4]
                .clone(),
        },
        6 => Input::PatchStatus {
            patch: patch_id(1 + u128::from(p.b % 2)),
            resolution: if p.d.is_multiple_of(2) {
                PatchResolution::Unmapped
            } else {
                PatchResolution::Mapped {
                    device: "d".into(),
                    channels: vec![0, 1],
                }
            },
        },
        _ => Input::Tick,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(256)))]

    /// After any random sequence of commands, undos and redos, mixed with runtime inputs and
    /// media and patch reports, an engine told only each edit's footprint
    /// (`model_changed_cues`) has done exactly what an engine that rebuilds everything
    /// (`model_changed`) has: the same actions on every call, the same broken cues and group
    /// warnings, and its bookkeeping equals a from-scratch rebuild (M4.12a).
    #[test]
    fn incremental_problems_equal_a_full_rebuild(sequence in picks(80)) {
        let config = EngineConfig::new(48_000, 0, 1).unwrap();
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        let (mut inc, mut full) = (CueEngine::new(config), CueEngine::new(config));
        let mut now = 0;
        let mut call = |inc: &mut CueEngine, full: &mut CueEngine, ws: &Workspace, input: Input| {
            now += 480;
            let (a, b) = (inc.handle(ws, input.clone(), now), full.handle(ws, input, now));
            prop_assert_eq!(a, b);
            prop_assert_eq!(inc.unsignalled_model_changes(), 0);
            inc.verify_problem_cache(ws).map_err(TestCaseError::fail)?;
            prop_assert_eq!(inc.broken_cues(), full.broken_cues());
            for id in all_ids(ws) {
                prop_assert_eq!(inc.state(id), full.state(id));
                prop_assert_eq!(inc.has_broken_descendant(id), full.has_broken_descendant(id));
            }
            Ok(())
        };
        inc.model_changed();
        full.model_changed();
        call(&mut inc, &mut full, &ws, Input::Tick)?;
        for p in &sequence {
            if let Some(fp) = random_edit(&mut ws, &mut g, &mut history, p) {
                // As the control thread does: the cue-scoped shortcut only for cue edits.
                if fp.is_model_wide() {
                    inc.model_changed();
                } else {
                    inc.model_changed_cues(&fp);
                }
                full.model_changed();
                call(&mut inc, &mut full, &ws, Input::Tick)?;
            }
            call(&mut inc, &mut full, &ws, status_input(&ws, p))?;
        }
    }
}

/// The property tests above mean nothing if nearly every random command is refused: check that
/// each command succeeds often enough on the same generator.
#[test]
fn the_generator_exercises_every_command() {
    use std::collections::HashMap;
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        // xorshift: deterministic, no clock or OS randomness
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 16) as u32
    };
    let mut applied: HashMap<u8, usize> = HashMap::new();
    let mut refused: HashMap<u8, usize> = HashMap::new();
    let mut dependent = 0;
    let mut readded = 0;
    for _ in 0..60 {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        for _ in 0..80 {
            let p = Pick {
                op: (next() % u32::from(OPS)) as u8,
                a: next(),
                b: next(),
                c: next(),
                d: next(),
            };
            let Some(command) = build(&ws, &mut g, p.op, &p) else {
                continue;
            };
            if let Command::Transaction { commands } = &command {
                // An inner command that touches a cue an earlier one added.
                let mut added = std::collections::HashSet::new();
                if let [Command::RemoveCue { id: a }, Command::AddCue { cue, .. }] =
                    commands.as_slice()
                    && cue.id == *a
                {
                    readded += 1;
                }
                for inner in commands {
                    let fp = inner.footprint();
                    if fp
                        .removed
                        .iter()
                        .chain(&fp.moved)
                        .chain(&fp.edited)
                        .any(|id| added.contains(id))
                    {
                        dependent += 1;
                        break;
                    }
                    added.extend(fp.added);
                }
            }
            let tally = if ws.apply(command).is_ok() {
                &mut applied
            } else {
                &mut refused
            };
            *tally.entry(p.op).or_default() += 1;
        }
    }
    for op in 0..OPS {
        assert!(
            applied.get(&op).copied().unwrap_or(0) >= 20,
            "op {op}: applied {applied:?}, refused {refused:?}"
        );
    }
    assert!(
        dependent >= 5,
        "only {dependent} transactions with dependent commands"
    );
    assert!(
        readded >= 5,
        "only {readded} transactions re-adding a removed id"
    );
    // And invalid input is generated, not just valid.
    // (A transaction is built from commands that apply, so it is never refused.)
    for op in [
        0u8, 2, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 17, 19, 20, 21, 22,
    ] {
        assert!(
            refused.get(&op).copied().unwrap_or(0) >= 3,
            "op {op} never refused"
        );
    }
}

/// `readd` makes the shapes that are hard to follow: groups two deep that keep descendant ids
/// with other content, and ids that change from group to plain cue and back.
#[test]
fn readd_reuses_descendant_ids_with_changed_content_and_nests() {
    let nested = |n: u128| {
        common::cue(
            n,
            group(vec![
                common::cue(n + 1, group(vec![common::cue(n + 2, CueKind::Memo)])),
                common::cue(n + 3, CueKind::Memo),
            ]),
        )
    };
    let ws = common::workspace(vec![nested(10), nested(20), common::cue(1, CueKind::Memo)]);
    let mut g = Gen { next: 5000 };
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 16) as u32
    };
    let (mut nested_kept, mut changed_kept, mut to_plain, mut applied) = (0, 0, 0, 0);
    for _ in 0..600 {
        let p = Pick {
            op: 10,
            a: next(),
            b: next(),
            c: next(),
            d: next(),
        };
        let Some(commands) = readd(&ws, &mut g, &p) else {
            continue;
        };
        let [Command::RemoveCue { id }, Command::AddCue { cue, .. }] = commands.as_slice() else {
            panic!("not a re-add");
        };
        assert_eq!(cue.id, *id);
        applied += 1;
        let Some(old) = ws.find_cue(*id) else {
            continue;
        };
        let before: std::collections::HashMap<CueId, &Cue> =
            flat(old).into_iter().map(|c| (c.id, c)).collect();
        let kept: Vec<&Cue> = flat(cue)
            .into_iter()
            .filter(|c| c.id != *id && before.contains_key(&c.id))
            .collect();
        nested_kept += usize::from(depth(cue) >= 2 && !kept.is_empty());
        changed_kept += usize::from(kept.iter().any(|c| before[&c.id].kind != c.kind));
        to_plain += usize::from(depth(old) >= 1 && depth(cue) == 0);
    }
    assert!(applied > 100, "only {applied} re-adds apply");
    assert!(
        nested_kept >= 20,
        "nested re-adds keeping ids: {nested_kept}"
    );
    assert!(
        changed_kept >= 20,
        "kept ids with other content: {changed_kept}"
    );
    assert!(to_plain >= 5, "groups re-added as plain cues: {to_plain}");
}
