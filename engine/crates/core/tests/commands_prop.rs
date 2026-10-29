// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

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

/// Operations 0-10 edit the cue tree; 11 and up edit settings, patches, cue lists and the OSC
/// passcode table.
const OPS: u8 = 29;

/// Names and hosts a command takes or refuses: plain, empty, at and over the limits (256 scalars,
/// 253 bytes; multi-byte text is counted in scalars, so 256 emoji pass), with a newline, a NUL.
fn name_pool(n: u32) -> String {
    match n % 9 {
        0 => "Act 1".into(),
        1 => String::new(),
        2 => "n".repeat(256),
        3 => "n".repeat(257),
        4 => "\u{1F3AD}".repeat(256),
        5 => "\u{1F3AD}".repeat(257),
        6 => "two\nlines".into(),
        7 => "nul\0".into(),
        _ => format!("name {}", n % 5),
    }
}

fn host_pool(n: u32) -> String {
    match n % 6 {
        0 => format!("10.0.0.{}", n % 255),
        1 => "h".repeat(253),
        2 => "h".repeat(254),
        3 => "\u{e9}".repeat(126),
        4 => "host\tname".into(),
        _ => "localhost".into(),
    }
}

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
    match variant % 17 {
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
                hotkey: ["a", "f5", "Bad Key", "", "space"]
                    .get(p.b as usize % 6)
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
        // Valid and hostile network values alike: a refusal must leave nothing behind.
        14 => CueProperty::Destination {
            value: net_id(1 + u128::from(p.b % 4)),
        },
        15 => CueProperty::Address {
            value: ["/a", "/cue/*/go", "no slash", "", "/a b", "/ok/{x,y}"][p.b as usize % 6]
                .into(),
        },
        16 => CueProperty::Args {
            value: match p.b % 5 {
                0 => vec![],
                1 => vec![OscArg::Int { value: p.c as i32 }],
                2 => vec![OscArg::Float { value: f32::NAN }],
                3 => vec![OscArg::String { value: "s".into() }; 1 + (p.c % 40) as usize],
                _ => vec![OscArg::Bool { value: true }, OscArg::Float { value: 0.25 }],
            },
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
        TransportKey::LoadSelected,
        TransportKey::PauseResumeSelected,
        TransportKey::StartSelected,
        TransportKey::StopSelected,
        TransportKey::ResetSelected,
    ][p.c as usize % 11];
    match p.b % 8 {
        0 => WorkspaceSetting::MinGoInterval {
            value: time(p.c) / 10.0,
        },
        1 => WorkspaceSetting::PanicDuration { value: time(p.c) },
        2 => WorkspaceSetting::AutosaveInterval {
            value: time(p.c) * 10.0,
        },
        3 => WorkspaceSetting::MasterDb { value: level(p.c) },
        4 | 5 => WorkspaceSetting::Hotkey {
            key,
            // "escape" is reserved and "l" / "space" are default keys: refusals (M5.11c).
            value: ["a", "f5", "Bad Key", "", "escape", "l", "space"]
                .get(p.d as usize % 8)
                .map(|k| Hotkey {
                    key: (*k).into(),
                    modifiers: Modifiers::default(),
                }),
        },
        6 => WorkspaceSetting::AutoNumberStart { value: time(p.c) },
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
            value: name_pool(p.d),
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
            value: name_pool(p.d),
        },
        1 => NetworkPatchProperty::Host {
            value: host_pool(p.d),
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

/// Settings, patches, network patches, cue lists and the OSC passcode table: operations 11 to 23
/// and 25 to 27. Ids are drawn from a
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
            new.name = name_pool(p.a);
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
                name: name_pool(p.d),
                host: host_pool(p.a),
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
            name: name_pool(p.d),
        },
        21 => Command::RemoveCueList { id: some_list() },
        22 => Command::MoveCueList {
            id: some_list(),
            index: p.b as usize % (ws.cue_lists.len() + 1),
        },
        23 => Command::RenameCueList {
            id: some_list(),
            name: name_pool(p.d),
        },
        28 => Command::RenameWorkspace {
            name: name_pool(p.d),
        },
        // The OSC passcode table. The pool is small, so passcodes repeat (refused), go missing
        // (refused) and come back; some are not digits and some rights break the View rule.
        25 => Command::AddOscPasscode {
            index: p.b as usize % (ws.settings.osc_access.passcodes.len() + 2),
            passcode: osc_passcode(p.c),
            rights: osc_rights(p.d),
        },
        26 => Command::RemoveOscPasscode {
            passcode: osc_passcode(p.c),
        },
        27 => Command::SetOscRights {
            target: if p.b.is_multiple_of(3) {
                OscRightsTarget::NoPasscode
            } else {
                OscRightsTarget::Passcode {
                    passcode: osc_passcode(p.c),
                }
            },
            rights: osc_rights(p.d),
        },
        _ => return None,
    })
}

fn osc_passcode(n: u32) -> String {
    ["1111", "2222", "3333", "0", "12ab", ""][n as usize % 6].to_owned()
}

fn osc_rights(n: u32) -> OscRights {
    OscRights {
        view: !n.is_multiple_of(7),
        edit: n.is_multiple_of(3),
        control: n.is_multiple_of(5),
    }
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
            property: property(ws, (p.d % 17) as u8, p),
        },
        7 => {
            let selected = (0..1 + p.c % 3).filter_map(|n| choose(&ids, p.a.wrapping_add(n * 7)));
            Command::BatchSet {
                ids: selected.collect(),
                // Every variant but `Kind` (11).
                property: property(
                    ws,
                    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15, 16][p.d as usize % 16],
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
        100..=116 => Command::SetCueProperty {
            id: id?,
            property: property(ws, op - 100, p),
        },
        24 => {
            // The multi-select gesture: several moves, or a Target and a Kind of one cue, in one
            // command. Each is built against what the earlier ones leave.
            let mut evolving = ws.clone();
            let mut commands = Vec::new();
            for n in 0..2 + p.c % 2 {
                let pick = p.a.wrapping_add(n * 17);
                let candidate = match p.b % 4 {
                    // A ganged drag: two outputs of a patch, or two settings, at once.
                    3 => Some(if p.c.is_multiple_of(2) {
                        Command::SetPatchProperty {
                            id: patch_id(1 + u128::from(p.a % 2)),
                            property: PatchProperty::OutputLevel {
                                output: (n % 2) as usize,
                                value: level(p.d.wrapping_add(n)),
                            },
                        }
                    } else {
                        Command::SetWorkspaceSetting {
                            setting: if n % 2 == 0 {
                                WorkspaceSetting::MasterDb { value: level(p.d) }
                            } else {
                                WorkspaceSetting::PanicDuration { value: time(p.d) }
                            },
                        }
                    }),
                    0 | 1 => {
                        let id = choose(&all_ids(&evolving), pick);
                        id.and_then(|id| {
                            Some(Command::MoveCue {
                                id,
                                index: (p.d.wrapping_add(n) as usize)
                                    % (siblings_of(&evolving, id)? + 1),
                            })
                        })
                    }
                    _ => {
                        let id = choose(&all_ids(&evolving), p.a)?;
                        Some(if n % 2 == 0 {
                            Command::SetCueProperty {
                                id,
                                property: property(&evolving, 9, p),
                            }
                        } else {
                            Command::SetCueProperty {
                                id,
                                property: CueProperty::Kind {
                                    value: kind(&evolving, p),
                                },
                            }
                        })
                    }
                };
                if let Some(command) = candidate
                    && evolving.apply(command.clone()).is_ok()
                {
                    commands.push(command);
                }
            }
            Command::Transaction { commands }
        }
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

/// What commands may leave behind: broken references, silent extra outputs, playlist-only
/// second triggers on other cues and cue hotkeys the workspace uses, never a value
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
                    // Kept as QLab keeps it; the workspace's key wins (M2.28, check K1b).
                    | IssueKind::HotkeyUsedByWorkspace { .. }
                    // A cue may name a patch the workspace lacks (it was removed with
                    // `even_if_used`, or the cue was added after): the broken-cue state.
                    | IssueKind::PatchMissing { .. }
                    | IssueKind::NetworkPatchMissing { .. }
                    // A transport key name is kept as given, like load does (it only warns).
                    | IssueKind::InvalidHotkey { .. }
                    // A loaded odd name or host is kept (hostile_base); commands refuse new ones.
                    | IssueKind::InvalidName { .. }
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
    let mut history = History::new();
    // Undo and redo go through `History`, which restores values that load tolerates (a Network
    // cue whose patch was removed on purpose) without the checks a client's command gets.
    let Ok(changed) = history.execute(&mut work, command.clone()) else {
        prop_assert_eq!(&work, ws);
        prop_assert_eq!(bytes(&work), bytes(ws));
        return Ok(false);
    };
    // An empty-inverse command is not recorded; the plain apply decides what it did.
    if !changed {
        let mut plain = ws.clone();
        plain
            .apply(command)
            .map_err(|e| TestCaseError::fail(format!("{e}")))?;
        prop_assert_eq!(&work, &plain);
        return Ok(true);
    }
    let after = work.clone();
    check_invariants(&after)?;
    history
        .undo(&mut work)
        .map_err(|e| TestCaseError::fail(format!("undo: {e}")))?;
    prop_assert_eq!(&work, ws);
    prop_assert_eq!(bytes(&work), bytes(ws));
    history
        .redo(&mut work)
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
            #![proptest_config(ProptestConfig::with_cases(prop_cases(64)))]
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
    set_destination_round_trips: 114,
    set_address_round_trips: 115,
    set_args_round_trips: 116,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(prop_cases(128)))]

    /// `BatchSet` is applied in place with rollback. The oracle is the plain `Transaction` of the
    /// same sets, which rolls back with the inverses of the sets it applied: result, inverse and workspace bytes must
    /// match whether the batch applies or one cue (a missing id, a cue without the property, an
    /// invalid value halfway) refuses it, and undo and redo must restore exactly.
    #[test]
    fn batch_set_matches_the_snapshot_transaction(
        prefix in picks(30),
        p in pick(),
        len in 0usize..14,
        missing_at in proptest::option::of(0usize..14),
    ) {
        let (ws, _) = after_prefix(&prefix);
        let ids = all_ids(&ws);
        let mut selected: Vec<CueId> = (0..len as u32)
            .filter_map(|n| choose(&ids, p.a.wrapping_add(n.wrapping_mul(p.c | 1))))
            .collect();
        if let Some(at) = missing_at {
            selected.insert(at.min(selected.len()), cue_id(987_654));
        }
        let prop = property(&ws, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15, 16][p.d as usize % 16], &p);
        let oracle = Command::Transaction {
            commands: selected
                .iter()
                .map(|&id| Command::SetCueProperty { id, property: prop.clone() })
                .collect(),
        };
        let batch = Command::BatchSet { ids: selected, property: prop };

        let mut expected = ws.clone();
        let expected_result = expected.apply(oracle);
        let mut actual = ws.clone();
        let actual_result = actual.apply(batch.clone());
        prop_assert_eq!(&actual_result, &expected_result);
        prop_assert_eq!(bytes(&actual), bytes(&expected));
        if actual_result.is_err() {
            prop_assert_eq!(bytes(&actual), bytes(&ws));
        }
        check_command(&ws, batch)?;
    }


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

/// A command that is refused whatever the workspace holds.
fn always_refused(kind: u8) -> Command {
    match kind % 6 {
        0 => Command::RemoveCue {
            id: cue_id(987_654),
        },
        1 => Command::SetCueProperty {
            id: cue_id(987_654),
            property: CueProperty::Flagged { value: true },
        },
        2 => Command::Transaction { commands: vec![] },
        3 => Command::RemoveCueList {
            id: list_id(987_654),
        },
        4 => Command::AddPatch {
            index: usize::MAX,
            patch: patch(987_654, 2),
        },
        _ => Command::SetPatchProperty {
            id: patch_id(987_654),
            property: PatchProperty::MasterDb { value: 0.0 },
        },
    }
}

/// [`base`] plus what load accepts as it is but a command would refuse as a new value, put
/// through a show file: a network patch on port 0 that a cue uses, a legacy passcode that is
/// not digits (in the generator's pool, so it gets removed), cue and transport hotkeys load only
/// warns about, -0.0 values, a fade whose target is missing and an audio cue on a missing patch.
fn hostile_base() -> Workspace {
    let mut ws = base();
    ws.network_patches.push(NetworkPatch {
        id: net_id(3),
        name: "Zero".into(),
        host: "".into(),
        port: 0,
        protocol: NetworkProtocol::Udp,
    });
    ws.settings.osc_access.passcodes.push(OscPasscode {
        passcode: "12ab".into(),
        rights: OscRights {
            view: true,
            edit: true,
            control: false,
        },
    });
    ws.settings.hotkeys.go = Some(Hotkey {
        key: "Space Bar".into(),
        modifiers: Modifiers::default(),
    });
    ws.settings.master_db = -0.0;
    // Names load keeps with a warning, and an edit away from them is undone back to them.
    ws.name = "w".repeat(300);
    ws.cue_lists[1].name = "two\nlines".into();
    ws.patches[1].name = "\u{1F3AD}".repeat(300);
    ws.network_patches[0].name = "nul\0".into();
    ws.network_patches[0].host = "h".repeat(300);
    let mut hot = common::cue(901, CueKind::Memo);
    hot.triggers.hotkey = Some(Hotkey {
        key: "A".into(),
        modifiers: Modifiers::default(),
    });
    hot.pre_wait = -0.0;
    let mut net = common::cue(
        902,
        CueKind::Network {
            destination: net_id(3),
            address: "/go".into(),
            args: vec![],
        },
    );
    net.number = Some("12".into());
    let mut lost = common::cue(903, audio());
    if let CueKind::Audio { patch, .. } = &mut lost.kind {
        *patch = patch_id(77);
    }
    ws.cue_lists[0].cues = vec![hot, net, common::cue(904, fade(999)), lost];
    let loaded = load_show(&show_text(&ws)).unwrap().workspace;
    assert_eq!(loaded.network_patches.last().unwrap().port, 0);
    assert_eq!(loaded.settings.osc_access.passcodes.len(), 1);
    assert!(
        loaded
            .find_cue(cue_id(901))
            .unwrap()
            .triggers
            .hotkey
            .is_some()
    );
    loaded
}

/// A workspace and id generator after the random `prefix`, from [`base`] or [`hostile_base`].
fn after_prefix_from(hostile: bool, prefix: &[Pick]) -> (Workspace, Gen) {
    let (mut ws, mut g) = (
        if hostile { hostile_base() } else { base() },
        Gen { next: 1000 },
    );
    for p in prefix {
        if let Some(command) = build(&ws, &mut g, p.op, p) {
            let _ = ws.apply(command);
        }
    }
    (ws, g)
}

/// Random edits of every kind, each built against the state the earlier ones leave, as one
/// transaction's commands. A built transaction is mostly spliced in (its commands were built
/// in order); kept whole it is a nested transaction, refused. Returns the commands and the
/// index of the first one that is refused, if any.
fn transaction_commands(
    ws: &Workspace,
    g: &mut Gen,
    inner: &[Pick],
    keep_refused: bool,
) -> (Vec<Command>, Option<usize>) {
    let mut evolving = ws.clone();
    let mut commands: Vec<Command> = Vec::new();
    let mut first_refused = None;
    for p in inner {
        let Some(built) = build(&evolving, g, p.op, p) else {
            continue;
        };
        let parts = match built {
            Command::Transaction { commands } if p.b % 4 != 0 => commands,
            other => vec![other],
        };
        for command in parts {
            let refused = matches!(command, Command::Transaction { .. })
                || evolving.apply(command.clone()).is_err();
            if refused && !keep_refused {
                continue;
            }
            if refused && first_refused.is_none() {
                first_refused = Some(commands.len());
            }
            commands.push(command);
        }
    }
    (commands, first_refused)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(prop_cases(256)))]

    /// A transaction refused at a random command is rolled back with the inverses of the
    /// commands before it (M1.19): the workspace is exactly as before, the error names the
    /// first refused command, and the history records nothing. A command that is always
    /// refused is put in at a random place; a random edit may be refused earlier. Half start
    /// from a loaded show holding values a command would refuse ([`hostile_base`]).
    #[test]
    fn a_refused_transaction_rolls_back_exactly(
        hostile in any::<bool>(),
        prefix in picks(30),
        inner in picks(14),
        fail_at in any::<proptest::sample::Index>(),
        failure in any::<u8>(),
    ) {
        let (ws, mut g) = after_prefix_from(hostile, &prefix);
        let (mut commands, first_refused) = transaction_commands(&ws, &mut g, &inner, true);
        let at = fail_at.index(commands.len() + 1);
        commands.insert(at, always_refused(failure));
        let expected = first_refused.filter(|&first| first < at).unwrap_or(at);

        let mut work = ws.clone();
        let mut history = History::new();
        match history.execute(&mut work, Command::Transaction { commands }) {
            Err(CommandError::InTransaction { index, .. }) => prop_assert_eq!(index, expected),
            other => prop_assert!(false, "expected a refusal at {}, got {:?}", expected, other),
        }
        prop_assert_eq!(&work, &ws);
        prop_assert_eq!(format!("{work:?}"), format!("{ws:?}"));
        prop_assert_eq!(bytes(&work), bytes(&ws));
        prop_assert!(!history.can_undo() && !history.can_redo());
    }

    /// An undo step is a transaction applied as a restore. A random edit made behind the
    /// history's back may make it refused partway; then it is rolled back like any
    /// transaction: the workspace is exactly as before the undo and the step stays. Without
    /// the edit, undo and redo restore exactly.
    #[test]
    fn an_undo_refused_partway_changes_nothing(
        hostile in any::<bool>(),
        prefix in picks(30),
        inner in picks(14),
        meddle in proptest::option::of(pick()),
    ) {
        let (ws, mut g) = after_prefix_from(hostile, &prefix);
        let (commands, _) = transaction_commands(&ws, &mut g, &inner, false);
        let mut work = ws.clone();
        let mut history = History::new();
        let changed = history.execute(&mut work, Command::Transaction { commands }).unwrap();
        let after = work.clone();
        if let Some(p) = &meddle
            && let Some(command) = build(&work, &mut g, p.op, p)
        {
            let _ = work.apply(command);
        }
        let before = work.clone();
        match history.undo(&mut work) {
            Ok(undone) => {
                prop_assert_eq!(undone, changed);
                if before == after {
                    prop_assert_eq!(&work, &ws);
                    prop_assert_eq!(bytes(&work), bytes(&ws));
                    prop_assert_eq!(history.redo(&mut work).unwrap(), changed);
                    prop_assert_eq!(&work, &after);
                }
            }
            Err(error) => {
                prop_assert!(
                    !matches!(error, CommandError::RollbackFailed { .. }),
                    "{error}"
                );
                prop_assert_eq!(&work, &before);
                prop_assert_eq!(format!("{work:?}"), format!("{before:?}"));
                prop_assert_eq!(history.undo_len(), 1);
            }
        }
    }
}

/// Cases for the properties that hold incremental bookkeeping to its full rebuild: 256 for CI;
/// `OPENLAB_PROP_CASES=20000 cargo test --release -p openlab-core --test commands_prop` sweeps.
fn incremental_cases() -> u32 {
    prop_cases(256)
}

/// The footprint's touched and structural cues, as `CueIndex::reconcile` takes them.
fn reconcile_sets(
    fp: &Footprint,
) -> (
    std::collections::BTreeSet<CueId>,
    std::collections::BTreeSet<CueId>,
) {
    let structural: std::collections::BTreeSet<CueId> = fp
        .added
        .iter()
        .chain(&fp.removed)
        .chain(&fp.moved)
        .copied()
        .collect();
    let touched = structural.iter().chain(&fp.edited).copied().collect();
    (touched, structural)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(incremental_cases()))]

    /// A client that starts from the workspace and applies each edit's `model_changed` ends
    /// every step with exactly the engine's workspace: commands (groups moved and deleted with
    /// children, renumbering, transactions), undo and redo alike (ED-07, NFR-01). Read through a
    /// cue index that follows the footprints (M4.12c), the changes are the same as the walk's.
    #[test]
    fn model_changes_keep_a_mirror_identical(sequence in picks(80)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut mirror = ws.clone();
        let mut history = History::new();
        let mut index = CueIndex::build(&ws);
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
            let (touched, structural) = reconcile_sets(&fp);
            index.reconcile(&ws, &touched, &structural, &fp.added);
            let indexed = model_changes_indexed(&ws, &fp, &index);
            prop_assert_eq!(indexed, Some((upserts.clone(), deletes.clone())));
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
    #![proptest_config(ProptestConfig::with_cases(incremental_cases()))]

    /// The cue index follows every edit from its footprint alone and equals one built from
    /// scratch after each (commands, undo, redo, transactions that add, move and remove), its
    /// positions included (M4.12c); what it answers about order and lists is the workspace's.
    #[test]
    fn the_cue_index_follows_footprints(sequence in picks(80)) {
        let (mut ws, mut g) = (base(), Gen { next: 1000 });
        let mut history = History::new();
        let mut index = CueIndex::build(&ws);
        for p in &sequence {
            let Some(fp) = random_edit(&mut ws, &mut g, &mut history, p) else { continue };
            let (touched, structural) = reconcile_sets(&fp);
            let change = index.reconcile(&ws, &touched, &structural, &fp.added);
            // Only an edit of the cue lists themselves may make the index start over.
            prop_assert!(matches!(change, IndexChange::Delta(_)) || !fp.lists.is_empty());
            prop_assert!(index.matches(&ws), "index differs from a rebuild");
            for list in &ws.cue_lists {
                let order = list_positions(&list.cues);
                prop_assert_eq!(&index.list_order(list.id), &order);
                for (i, id) in order.iter().enumerate() {
                    prop_assert_eq!(index.list_of(*id), Some(list.id));
                    prop_assert_eq!(index.next_in_list(*id), order.get(i + 1).copied());
                    prop_assert_eq!(index.locate(&ws, *id).map(|c| c.id), Some(*id));
                }
            }
        }
    }
}

/// Every cue of a list, depth first.
fn list_positions(cues: &[Cue]) -> Vec<CueId> {
    let mut out = Vec::new();
    for cue in cues {
        out.push(cue.id);
        out.extend(list_positions(cue.children()));
    }
    out
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
        // The playhead stands anywhere (M4.12c follows it from footprints), GO walks on from
        // there, in and out of groups (M2.24 group visits).
        7 => Input::PlayheadNext { list },
        8 => Input::PlayheadPrevious { list },
        9 => Input::SetPlayhead {
            list,
            cue: choose(&ids, p.b).filter(|_| !p.d.is_multiple_of(5)),
        },
        10 => Input::Go { list },
        _ => Input::Tick,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(incremental_cases()))]

    /// After any random sequence of commands, undos and redos, mixed with runtime inputs,
    /// playhead moves and media and patch reports, an engine told only each edit's footprint
    /// (`model_changed_cues`) has done exactly what an engine that rebuilds everything
    /// (`model_changed`) has: the same actions on every call (playhead moves and group visits
    /// included), the same playheads, broken cues and group warnings, and its bookkeeping
    /// equals a from-scratch rebuild (M4.12a). Some edits come two or more to a call, as
    /// footprints the engine adds up. Following footprints never walks the whole show for the
    /// chain limit, the playheads or the cues present (M4.12c).
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
            for list in &ws.cue_lists {
                prop_assert_eq!(inc.playhead(list.id), full.playhead(list.id));
            }
            for id in all_ids(ws) {
                prop_assert_eq!(inc.state(id), full.state(id));
                prop_assert_eq!(inc.has_broken_descendant(id), full.has_broken_descendant(id));
            }
            Ok(())
        };
        inc.model_changed();
        full.model_changed();
        call(&mut inc, &mut full, &ws, Input::Tick)?;
        let passes = inc.whole_show_passes();
        // An edit of settings, patches or lists is told with the full `model_changed`, which
        // walks the show by design: the no-extra-passes property is for cue edits.
        let mut wide = false;
        for p in &sequence {
            if let Some(fp) = random_edit(&mut ws, &mut g, &mut history, p) {
                // As the control thread does: the cue-scoped shortcut only for cue edits.
                if fp.is_model_wide() {
                    wide = true;
                    inc.model_changed();
                } else {
                    inc.model_changed_cues(&fp);
                }
                full.model_changed();
                if p.b.is_multiple_of(4) {
                    // The next edit comes before the engine is called.
                    continue;
                }
                call(&mut inc, &mut full, &ws, Input::Tick)?;
            }
            call(&mut inc, &mut full, &ws, status_input(&ws, p))?;
        }
        call(&mut inc, &mut full, &ws, Input::Tick)?;
        if !wide {
            prop_assert_eq!(inc.whole_show_passes(), passes);
        }
    }
}

/// A show where group visits matter (M2.24): groups of every mode that continue (auto-follow,
/// auto-continue) with timed children, nested groups, and a second list.
fn visits_show() -> Workspace {
    let timed = |mut c: Cue, mode: ContinueMode| {
        c.continue_mode = mode;
        c
    };
    let wait = |n: u128| common::cue(n, CueKind::Wait { duration: 0.01 });
    let memo = |n: u128| common::cue(n, CueKind::Memo);
    let grp = |n: u128, mode: GroupMode, children: Vec<Cue>| {
        common::cue(n, CueKind::Group { mode, children })
    };
    let mut ws = base();
    ws.cue_lists[0].cues = vec![
        timed(
            grp(
                1,
                GroupMode::StartFirstAndEnter,
                vec![wait(10), memo(11), memo(12)],
            ),
            ContinueMode::AutoFollow,
        ),
        memo(2),
        timed(
            grp(
                3,
                GroupMode::Playlist {
                    loop_: false,
                    shuffle: false,
                },
                vec![wait(13), wait(14)],
            ),
            ContinueMode::AutoContinue,
        ),
        wait(4),
        timed(
            grp(
                5,
                GroupMode::Timeline,
                vec![
                    memo(15),
                    grp(6, GroupMode::StartFirstAndEnter, vec![wait(16), memo(17)]),
                ],
            ),
            ContinueMode::AutoFollow,
        ),
        memo(7),
        wait(8),
    ];
    ws.cue_lists[1].cues = vec![memo(20), wait(21)];
    ws
}

/// An edit aimed at the playhead (its cue moved into or out of a group, removed, removed and
/// added again) or any random edit, undo or redo.
fn edit_near_playhead(
    ws: &mut Workspace,
    g: &mut Gen,
    history: &mut History,
    p: &Pick,
    head: Option<CueId>,
) -> Option<Footprint> {
    let target = head.filter(|_| !p.c.is_multiple_of(3));
    let command = match (p.op % 5, target) {
        (0, Some(id)) => Command::ReparentCue {
            id,
            to: position(ws, p.b, p.d),
        },
        (1, Some(id)) => Command::RemoveCue { id },
        (2, Some(id)) => Command::MoveCue {
            id,
            index: p.b as usize % 4,
        },
        (3, _) => Command::Transaction {
            commands: readd(ws, g, p)?,
        },
        _ => return random_edit(ws, g, history, p),
    };
    let fp = command.footprint();
    history.execute(ws, command).is_ok().then_some(fp)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(incremental_cases()))]

    /// Playheads and group visits followed from footprints (M4.12c) do exactly what the full
    /// rebuild does, on a show built for it: GO into and out of groups that continue while time
    /// passes, the playhead set anywhere, and edits aimed at the playhead cue and the groups
    /// around it (moved in and out of groups and between lists, removed, removed and added
    /// again under its id), undo and redo, some added up before a call.
    #[test]
    fn playheads_and_group_visits_follow_footprints(sequence in picks(60)) {
        let config = EngineConfig::new(1000, 0, 1).unwrap();
        let mut ws = visits_show();
        let mut g = Gen { next: 1000 };
        let mut history = History::new();
        let (mut inc, mut full) = (CueEngine::new(config), CueEngine::new(config));
        let mut now = 0;
        let mut call = |inc: &mut CueEngine, full: &mut CueEngine, ws: &Workspace, input: Input, dt: u64| {
            now += dt;
            let (a, b) = (inc.handle(ws, input.clone(), now), full.handle(ws, input, now));
            prop_assert_eq!(a, b);
            prop_assert_eq!(inc.unsignalled_model_changes(), 0);
            for list in &ws.cue_lists {
                prop_assert_eq!(inc.playhead(list.id), full.playhead(list.id));
            }
            Ok(())
        };
        inc.model_changed();
        full.model_changed();
        call(&mut inc, &mut full, &ws, Input::Tick, 1)?;
        let passes = inc.whole_show_passes();
        // An edit of settings, patches or lists is told with the full `model_changed`, which
        // walks the show by design: the no-extra-passes property is for cue edits.
        let mut wide = false;
        for p in &sequence {
            // A random edit can remove a list; address whichever lists remain.
            let Some(target) = ws.cue_lists.get(p.a as usize % ws.cue_lists.len().max(1)) else {
                break;
            };
            let list = target.id;
            let ids = all_ids(&ws);
            let dt = u64::from(p.d % 25);
            match p.c % 6 {
                0 | 1 => call(&mut inc, &mut full, &ws, Input::Go { list }, dt)?,
                2 => {
                    let cue = choose(&ids, p.b);
                    call(&mut inc, &mut full, &ws, Input::SetPlayhead { list, cue }, dt)?;
                }
                3 => call(&mut inc, &mut full, &ws, Input::PlayheadNext { list }, dt)?,
                _ => {
                    let head = inc.playhead(list);
                    if let Some(fp) = edit_near_playhead(&mut ws, &mut g, &mut history, p, head) {
                        if fp.is_model_wide() {
                            wide = true;
                            inc.model_changed();
                        } else {
                            inc.model_changed_cues(&fp);
                        }
                        full.model_changed();
                        if !p.b.is_multiple_of(5) {
                            call(&mut inc, &mut full, &ws, Input::Tick, dt)?;
                        }
                    }
                }
            }
        }
        call(&mut inc, &mut full, &ws, Input::Tick, 1)?;
        if !wide {
            prop_assert_eq!(inc.whole_show_passes(), passes);
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
    for _ in 0..120 {
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
        0u8, 2, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 17, 19, 20, 21, 22, 23, 25, 26, 27, 28,
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
