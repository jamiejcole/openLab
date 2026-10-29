// SPDX-License-Identifier: GPL-3.0-or-later

use super::super::gate::*;
use super::super::{ProbeClock, ProbeFn};
use super::*;
use crate::control::DocId;
use crate::control::inbox::Inbox;
use crate::control::watch::bucket_of;
use crossbeam_channel::Receiver;

use openlab_audio::{Media, MediaError, Probed};
use openlab_audio::{MediaInfo, Seeking};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn fake(_: &Path) -> Result<Probed, MediaError> {
    Ok(Probed {
        info: MediaInfo {
            channels: 1,
            sample_rate: 1000,
            frames: 1000,
            duration: Duration::from_secs(1),
        },
        warnings: vec![],
        seeking: Seeking::Direct,
    })
}

fn answer(rx: &Receiver<Inbox>) -> ProbeAnswer {
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Inbox::Probe(_, a) => a,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reprobe_keeps_the_last_answer_until_the_new_one() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut cat = MediaCatalog::with_own_pool(
        tx,
        None,
        0,
        fake,
        Duration::from_secs(30),
        ProbeClock::system(),
    )
    .unwrap();
    let key = "/abs/a.wav";
    cat.request(key.to_owned());
    assert_eq!(cat.pending(), 1);
    let a = answer(&rx);
    assert!(cat.answer(a).is_some());
    let Some(Probe::Ready(first)) = cat.get(key) else {
        panic!("not ready")
    };
    let first = Arc::clone(first);
    cat.reprobe(key);
    assert_eq!(cat.pending(), 1);
    // Meanwhile the last answer stands: the same probe, so primes built on it stay valid.
    let Some(Probe::Ready(still)) = cat.get(key) else {
        panic!("dropped the last answer")
    };
    assert!(Arc::ptr_eq(&first, still));
    let a = answer(&rx);
    assert!(cat.answer(a).is_some());
    assert_eq!(cat.pending(), 0);
}

// ----- following edits from their footprints (M4.12a) and preload changes (M4.12b) -----

use openlab_core::{Command, CueProperty, GroupMode, History, Position, load_show};
use std::collections::BTreeMap;

/// A mono 32-bit float WAV of 100 frames (400 bytes preloaded).
fn write_wav(dir: &Path, name: &str) -> String {
    write_wav_frames(dir, name, 100)
}

/// A mono 32-bit float WAV of `frames` frames at 8 kHz.
fn write_wav_frames(dir: &Path, name: &str, frames: u32) -> String {
    let data_len = frames * 4;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&32_000u32.to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0);
    let path = dir.join(name);
    std::fs::write(&path, v).unwrap();
    // The catalog keys files by `MediaRef`'s portable form (`/` on Windows too).
    MediaRef::new(path.to_str().unwrap()).as_str().to_owned()
}

fn catalog(budget: u64) -> (MediaCatalog, Receiver<Inbox>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let probe: ProbeFn = openlab_audio::probe;
    let cat = MediaCatalog::with_own_pool(
        tx,
        None,
        budget,
        probe,
        Duration::from_secs(60),
        ProbeClock::system(),
    )
    .unwrap();
    (cat, rx)
}

/// Hands the catalog the answers of every job it has sent, in a fixed order (the two
/// workers answer in any order, and the budget goes to whoever is answered first), until
/// nothing is in flight: answers can start preloads.
fn settle(cat: &mut MediaCatalog, rx: &Receiver<Inbox>, received: &mut u64) {
    while *received < cat.jobs {
        let mut round = Vec::new();
        while *received < cat.jobs {
            round.push(answer(rx));
            *received += 1;
        }
        round.sort_by_key(|a| match a {
            ProbeAnswer::Probed { key, .. } => (key.clone(), 0),
            ProbeAnswer::Preloaded { key, .. } => (key.clone(), 1),
        });
        for a in round {
            cat.answer(a);
        }
    }
}

/// Applies the catalog's key changes to `watched` as the watch would, and checks the watch
/// then watches exactly the catalog's files, each under its current watch id.
fn follow_keys(cat: &mut MediaCatalog, watched: &mut BTreeMap<String, u64>) {
    for change in cat.take_key_changes() {
        match change {
            KeyChange::Reset => watched.clear(),
            KeyChange::Watch { key, id, .. } => {
                assert!(watched.insert(key, id).is_none(), "watched twice");
            }
            KeyChange::Unwatch { key } => {
                assert!(watched.remove(&key).is_some(), "unwatched unknown");
            }
            KeyChange::ProbeFailed { key, id, .. } => {
                assert_eq!(
                    watched.get(&key),
                    Some(&id),
                    "a failed probe of a watched file"
                );
            }
        }
    }
    let expected: BTreeMap<String, u64> = cat
        .entries
        .iter()
        .map(|(k, e)| (k.clone(), e.watch))
        .collect();
    assert_eq!(*watched, expected);
}

#[test]
fn the_watch_follows_the_files_the_catalog_knows() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_wav(dir.path(), "a.wav");
    let b = write_wav(dir.path(), "b.wav");
    let (mut ws, template) = template();
    let mut c1 = template.clone();
    c1.id = CueId(uuid::Uuid::from_u128(901));
    c1.number = None;
    set_media(&mut c1, &a, false);
    let mut c2 = c1.clone();
    c2.id = CueId(uuid::Uuid::from_u128(902));
    set_media(&mut c2, &b, false);
    ws.cue_lists[0].cues = vec![c1, c2];
    let (mut cat, rx) = catalog(0);
    let mut got = 0;
    cat.sync(&ws);
    let mut watched = BTreeMap::new();
    follow_keys(&mut cat, &mut watched);
    assert_eq!(watched.len(), 2);
    settle(&mut cat, &rx, &mut got);
    // A re-probe is not a new watch.
    cat.reprobe(&a);
    assert!(cat.take_key_changes().is_empty());
    settle(&mut cat, &rx, &mut got);
    // A reset starts over: everything watched again under new ids.
    let old = watched.clone();
    cat.reset(None, &ws);
    let changes = cat.take_key_changes();
    assert_eq!(changes.first(), Some(&KeyChange::Reset));
    watched.clear();
    for c in changes.into_iter().skip(1) {
        let KeyChange::Watch { key, id, path } = c else {
            panic!("{c:?}")
        };
        assert_eq!(path, PathBuf::from(&key));
        watched.insert(key, id);
    }
    assert_eq!(watched.len(), 2);
    assert!(watched.iter().all(|(k, id)| old.get(k) != Some(id)));
    settle(&mut cat, &rx, &mut got);
    // A relative reference in a show never saved has no path: not watched.
    let mut c3 = template.clone();
    c3.id = CueId(uuid::Uuid::from_u128(903));
    set_media(&mut c3, "sounds/rel.wav", false);
    ws.cue_lists[0].cues.push(c3);
    cat.sync(&ws);
    assert!(cat.take_key_changes().is_empty());
    assert!(matches!(cat.get("sounds/rel.wav"), Some(Probe::Failed)));
}

/// A doc's files are resolved off the control thread, against its show folder (M4.15i);
/// an index resolved against another folder than the catalog's is resolved again.
#[test]
fn an_index_resolved_elsewhere_is_resolved_against_the_catalogs_folder() {
    let dir = tempfile::tempdir().unwrap();
    write_wav(dir.path(), "a.wav");
    let (mut ws, template) = template();
    let mut c = template.clone();
    c.id = CueId(uuid::Uuid::from_u128(901));
    set_media(&mut c, "a.wav", false);
    ws.cue_lists[0].cues = vec![c];
    let elsewhere = tempfile::tempdir().unwrap();
    for against in [dir.path(), elsewhere.path()] {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut cat = MediaCatalog::with_own_pool(
            tx,
            Some(dir.path().to_path_buf()),
            0,
            openlab_audio::probe,
            Duration::from_secs(60),
            ProbeClock::system(),
        )
        .unwrap();
        cat.sync_indexed(index_workspace(&ws, Some(against)));
        let watched: Vec<PathBuf> = cat
            .take_key_changes()
            .into_iter()
            .filter_map(|c| match c {
                KeyChange::Watch { path, .. } => Some(path),
                _ => None,
            })
            .collect();
        let want = media_path(&MediaRef::new("a.wav"), Some(dir.path())).unwrap();
        assert_eq!(watched, vec![want], "resolved against {against:?}");
        let mut got = 0;
        settle(&mut cat, &rx, &mut got);
        assert!(
            matches!(cat.get("a.wav"), Some(Probe::Ready(_))),
            "resolved against {against:?}"
        );
    }
}

#[test]
fn a_file_marked_missing_fails_at_once_and_ignores_its_probe_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_wav(dir.path(), "a.wav");
    let (mut ws, template) = template();
    let mut c1 = template.clone();
    c1.id = CueId(uuid::Uuid::from_u128(911));
    c1.number = None;
    set_media(&mut c1, &a, true);
    ws.cue_lists[0].cues = vec![c1];
    let (mut cat, rx) = catalog(10_000);
    let mut got = 0;
    cat.sync(&ws);
    let id = match cat.take_key_changes().as_slice() {
        [KeyChange::Watch { id, .. }] => *id,
        other => panic!("{other:?}"),
    };
    settle(&mut cat, &rx, &mut got);
    assert!(cat.is_preloaded(&a));
    assert_eq!(cat.preload_reserved(), 400);
    // An event of another watch of the key changes nothing.
    assert_eq!(cat.mark_missing(&a, id + 1000), None);
    assert!(!cat.recheck(&a, id + 1000));
    assert!(cat.ready(&a).is_some());
    // Gone while a re-probe is under way: failed now, the budget back, the answer ignored.
    cat.reprobe(&a);
    assert_eq!(
        cat.mark_missing(&a, id),
        Some((MediaRef::new(a.as_str()), MediaStatus::Missing))
    );
    assert!(matches!(cat.get(&a), Some(Probe::Failed)));
    assert!(!cat.is_preloaded(&a));
    assert_eq!(cat.preload_reserved(), 0);
    assert_eq!(cat.pending(), 0);
    assert!(cat.take_plan_changes().keys.contains(&a));
    let late = answer(&rx);
    got += 1;
    assert_eq!(
        cat.answer(late),
        None,
        "the answer of a probe before the loss"
    );
    assert!(matches!(cat.get(&a), Some(Probe::Failed)));
    // Back: probed afresh, and preloaded again.
    assert!(cat.recheck(&a, id));
    settle(&mut cat, &rx, &mut got);
    assert!(cat.ready(&a).is_some());
    assert!(cat.is_preloaded(&a));
    assert_eq!(cat.preload_reserved(), 400);
    // A file no cue uses any more is not marked.
    ws.cue_lists[0].cues.clear();
    cat.sync(&ws);
    assert_eq!(cat.mark_missing(&a, id), None);
}

/// Everything the catalog knows about its files, for comparing two catalogs.
type View = (
    u64,
    BTreeMap<String, (usize, usize, bool, bool, bool, bool, u64, u64, u64)>,
    BTreeMap<CueId, (String, bool)>,
);

fn view(cat: &MediaCatalog) -> View {
    let entries = cat
        .entries
        .iter()
        .map(|(k, e)| {
            (
                k.clone(),
                (
                    e.users,
                    e.preloaders,
                    e.wants_preload,
                    e.over_budget,
                    e.preload_failed,
                    e.preloaded.is_some(),
                    e.reserved,
                    e.ticket,
                    e.preload_ticket,
                ),
            )
        })
        .collect();
    let uses = cat
        .uses
        .iter()
        .map(|(id, u)| (*id, (u.key.clone(), u.preload)))
        .collect();
    (cat.reserved, entries, uses)
}

/// Random walks for the twin tests: `default` for CI, `OPENLAB_PROP_CASES` for a sweep.
fn sweep_seeds(default: u64) -> u64 {
    std::env::var("OPENLAB_PROP_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as usize % n.max(1)
    }
}

fn all_cues(ws: &Workspace) -> Vec<(CueId, bool)> {
    let mut out = Vec::new();
    for list in &ws.cue_lists {
        let mut stack: Vec<&[Cue]> = vec![&list.cues];
        while let Some(cues) = stack.pop() {
            for c in cues {
                out.push((c.id, matches!(c.kind, CueKind::Group { .. })));
                stack.push(c.children());
            }
        }
    }
    out
}

/// One random edit of `ws` built the way the tests of commands do: always plausible,
/// sometimes refused.
fn random_command(
    ws: &Workspace,
    rng: &mut Rng,
    template: &Cue,
    files: &[String],
    next: &mut u128,
) -> Command {
    let cues = all_cues(ws);
    let pick = |rng: &mut Rng| cues[rng.below(cues.len())];
    let groups: Vec<CueId> = cues.iter().filter(|c| c.1).map(|c| c.0).collect();
    let position = |rng: &mut Rng| Position {
        list: ws.cue_lists[rng.below(ws.cue_lists.len())].id,
        parent: if groups.is_empty() || rng.below(2) == 0 {
            None
        } else {
            Some(groups[rng.below(groups.len())])
        },
        index: rng.below(4),
    };
    let mut new_cue = |rng: &mut Rng, group: bool| {
        *next += 1;
        let mut c = template.clone();
        c.id = CueId(uuid::Uuid::from_u128(*next));
        c.number = None;
        c.kind = if group {
            *next += 1;
            let mut inner = template.clone();
            inner.id = CueId(uuid::Uuid::from_u128(*next));
            inner.number = None;
            set_media(
                &mut inner,
                &files[rng.below(files.len())],
                rng.below(2) == 0,
            );
            CueKind::Group {
                mode: GroupMode::Timeline,
                children: vec![inner],
            }
        } else {
            template.kind.clone()
        };
        if !group {
            set_media(&mut c, &files[rng.below(files.len())], rng.below(2) == 0);
        }
        c
    };
    // An empty show can only be added to.
    match if cues.is_empty() { 0 } else { rng.below(8) } {
        0 | 1 => Command::AddCue {
            at: position(rng),
            cue: new_cue(rng, false),
            auto_number: false,
        },
        2 => Command::AddCue {
            at: position(rng),
            cue: new_cue(rng, true),
            auto_number: false,
        },
        3 => Command::RemoveCue { id: pick(rng).0 },
        4 => Command::MoveCue {
            id: pick(rng).0,
            index: rng.below(4),
        },
        5 => Command::ReparentCue {
            id: pick(rng).0,
            to: position(rng),
        },
        6 => {
            // A cue removed and added again under its id: as a group with other cues, or
            // as a plain cue.
            let id = pick(rng).0;
            let group = rng.below(2) == 0;
            let mut cue = new_cue(rng, group);
            cue.id = id;
            Command::Transaction {
                commands: vec![
                    Command::RemoveCue { id },
                    Command::AddCue {
                        at: position(rng),
                        cue,
                        auto_number: false,
                    },
                ],
            }
        }
        _ => {
            // Another file and preload setting for a cue (whatever it was).
            let mut probe = template.clone();
            set_media(
                &mut probe,
                &files[rng.below(files.len())],
                rng.below(2) == 0,
            );
            Command::SetCueProperty {
                id: pick(rng).0,
                property: CueProperty::Kind { value: probe.kind },
            }
        }
    }
}

fn set_media(cue: &mut Cue, file: &str, want_preload: bool) {
    if let CueKind::Audio { media, preload, .. } = &mut cue.kind {
        *media = Some(MediaRef::new(file));
        *preload = want_preload;
    }
}

fn template() -> (Workspace, Cue) {
    let text = include_str!("../../../../../../fixtures/show/v1-example.openlab");
    let mut ws = load_show(text).unwrap().workspace;
    let id = openlab_core::audio_media(&ws)[0].0;
    let template = ws.find_cue(id).unwrap().clone();
    for list in &mut ws.cue_lists {
        list.cues.clear();
    }
    (ws, template)
}

/// An engine that follows each edit from its footprint keeps exactly the media catalog a
/// full `sync` of the workspace gives: the same files, counts, preload wishes, budget,
/// probes and preload jobs asked in the same order (tickets), whatever the edits, undos
/// and redos, and as the probe answers come in.
#[test]
fn following_footprints_equals_a_full_sync_after_any_edits() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<String> = (0..5)
        .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
        .collect();
    let (base, template) = template();
    let (mut preloaded, mut over, mut released, mut cleared) = (0, 0, 0, 0);
    for seed in 1..=sweep_seeds(30) {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        // Room for two of the five files (400 bytes each): the rest are over budget.
        let (mut inc, rx_inc) = catalog(900);
        let (mut full, rx_full) = catalog(900);
        let (mut got_inc, mut got_full) = (0, 0);
        let (mut ws, mut history, mut next) = (base.clone(), History::new(), 10_000u128);
        inc.sync(&ws);
        full.sync(&ws);
        let mut watched = BTreeMap::new();
        follow_keys(&mut inc, &mut watched);
        for step in 0..60 {
            let footprint = match rng.below(9) {
                0 => history
                    .peek_undo()
                    .map(openlab_core::Command::footprint)
                    .filter(|_| history.undo(&mut ws).unwrap_or(false)),
                1 => history
                    .peek_redo()
                    .map(openlab_core::Command::footprint)
                    .filter(|_| history.redo(&mut ws).unwrap_or(false)),
                _ => {
                    let command = random_command(&ws, &mut rng, &template, &files, &mut next);
                    let footprint = command.footprint();
                    history.execute(&mut ws, command).ok().map(|_| footprint)
                }
            };
            let Some(footprint) = footprint else { continue };
            let before = view(&inc);
            inc.sync_cues(&ws, &footprint);
            full.sync(&ws);
            full.take_key_changes();
            follow_keys(&mut inc, &mut watched);
            assert!(inc.index.matches(&ws), "seed {seed} step {step}");
            assert_eq!(view(&inc), view(&full), "seed {seed} step {step}");
            settle(&mut inc, &rx_inc, &mut got_inc);
            settle(&mut full, &rx_full, &mut got_full);
            let after = view(&inc);
            assert_eq!(after, view(&full), "seed {seed} step {step} after answers");
            // What the walk is meant to reach.
            preloaded += after.1.values().filter(|e| e.5).count();
            over += after.1.values().filter(|e| e.3).count();
            released += usize::from(before.0 > after.0);
            cleared += before
                .1
                .values()
                .filter(|e| e.3)
                .count()
                .saturating_sub(after.1.values().filter(|e| e.3).count());
        }
    }
    assert!(preloaded > 100, "preloads reached: {preloaded}");
    assert!(over > 20, "over-budget files reached: {over}");
    assert!(released > 20, "budget given back: {released}");
    assert!(
        cleared > 5,
        "over-budget flags cleared by a release: {cleared}"
    );
}

/// A group removed and added again under its id in one edit: the new group's file is
/// probed and counted, the old one's is forgotten (with its preload), whether the child
/// has a new id or keeps its own with another file.
#[test]
fn a_group_replaced_under_its_id_moves_its_files_users_to_the_new_children() {
    replaced_group(false);
    replaced_group(true);
}

fn replaced_group(same_child_id: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (old_file, new_file) = (
        write_wav(dir.path(), "old.wav"),
        write_wav(dir.path(), "new.wav"),
    );
    let (mut ws, template) = template();
    let mut old_child = template.clone();
    set_media(&mut old_child, &old_file, true);
    let mut new_child = template.clone();
    if !same_child_id {
        new_child.id = CueId(uuid::Uuid::from_u128(10_003));
    }
    set_media(&mut new_child, &new_file, true);
    let group = |id: u128, child: Cue| Cue {
        id: CueId(uuid::Uuid::from_u128(id)),
        kind: CueKind::Group {
            mode: GroupMode::Timeline,
            children: vec![child],
        },
        number: None,
        ..template.clone()
    };
    ws.cue_lists[0].cues = vec![group(10_001, old_child)];
    let (mut inc, rx_inc) = catalog(10_000);
    let (mut full, rx_full) = catalog(10_000);
    let (mut got_inc, mut got_full) = (0, 0);
    inc.sync(&ws);
    full.sync(&ws);
    settle(&mut inc, &rx_inc, &mut got_inc);
    settle(&mut full, &rx_full, &mut got_full);
    assert_eq!(inc.preload_reserved(), 400);
    let command = Command::Transaction {
        commands: vec![
            Command::RemoveCue {
                id: CueId(uuid::Uuid::from_u128(10_001)),
            },
            Command::AddCue {
                at: Position {
                    list: ws.cue_lists[0].id,
                    parent: None,
                    index: 0,
                },
                cue: group(10_001, new_child),
                auto_number: false,
            },
        ],
    };
    let footprint = command.footprint();
    ws.apply(command).unwrap();
    inc.sync_cues(&ws, &footprint);
    full.sync(&ws);
    assert!(inc.index.matches(&ws));
    assert_eq!(view(&inc), view(&full));
    settle(&mut inc, &rx_inc, &mut got_inc);
    settle(&mut full, &rx_full, &mut got_full);
    assert_eq!(view(&inc), view(&full));
    assert!(
        inc.get(&new_file).is_some(),
        "the new child's file is probed"
    );
    assert!(
        inc.get(&old_file).is_none(),
        "the old child's file is forgotten"
    );
    assert_eq!(inc.preload_reserved(), 400, "one preload, the new file's");
}

#[test]
fn switching_preload_on_for_a_probed_file_preloads_it_and_off_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_wav(dir.path(), "a.wav");
    let (mut ws, template) = template();
    let mut cue = template.clone();
    set_media(&mut cue, &file, false);
    let mut other = cue.clone();
    other.id = CueId(uuid::Uuid::from_u128(10_002));
    let id = cue.id;
    ws.cue_lists[0].cues = vec![cue, other];
    let (mut cat, rx) = catalog(10_000);
    let mut got = 0;
    cat.sync(&ws);
    settle(&mut cat, &rx, &mut got);
    assert_eq!(cat.preload_reserved(), 0, "nothing asks for a preload");
    assert!(cat.get(&file).is_some_and(|p| matches!(p, Probe::Ready(_))));

    let set = |ws: &mut Workspace, id: CueId, on: bool| {
        let mut c = ws.find_cue(id).unwrap().clone();
        set_media(&mut c, &file, on);
        let command = Command::SetCueProperty {
            id,
            property: CueProperty::Kind { value: c.kind },
        };
        let footprint = command.footprint();
        ws.apply(command).unwrap();
        footprint
    };
    // On: the file is already probed, and is now preloaded (it used to wait for the next
    // open).
    let fp = set(&mut ws, id, true);
    cat.sync_cues(&ws, &fp);
    assert_eq!(cat.preload_reserved(), 400);
    settle(&mut cat, &rx, &mut got);
    assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
    // A second cue asks too; the first stops asking: the file stays preloaded.
    let other_id = CueId(uuid::Uuid::from_u128(10_002));
    let fp = set(&mut ws, other_id, true);
    cat.sync_cues(&ws, &fp);
    let fp = set(&mut ws, id, false);
    cat.sync_cues(&ws, &fp);
    assert_eq!(cat.preload_reserved(), 400);
    assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
    // The last one stops: the reservation and the samples are released.
    let fp = set(&mut ws, other_id, false);
    cat.sync_cues(&ws, &fp);
    assert_eq!(cat.preload_reserved(), 0);
    assert!(matches!(cat.media(&file), Some((_, Media::Path(_)))));
    // And on again: a new preload.
    let fp = set(&mut ws, id, true);
    cat.sync_cues(&ws, &fp);
    assert_eq!(cat.preload_reserved(), 400);
    settle(&mut cat, &rx, &mut got);
    assert!(matches!(cat.media(&file), Some((_, Media::Preloaded(_)))));
}

#[test]
fn a_preload_answer_that_arrives_after_its_release_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_wav(dir.path(), "a.wav");
    let (mut ws, template) = template();
    let mut cue = template.clone();
    set_media(&mut cue, &file, true);
    let id = cue.id;
    ws.cue_lists[0].cues = vec![cue];
    let (mut cat, rx) = catalog(10_000);
    let mut got = 0;
    cat.sync(&ws);
    // The probe answers, which starts the preload job; before its answer is read, the
    // cue stops asking.
    let first = answer(&rx);
    got += 1;
    cat.answer(first);
    assert_eq!(cat.preload_reserved(), 400);
    let mut c = ws.find_cue(id).unwrap().clone();
    set_media(&mut c, &file, false);
    let command = Command::SetCueProperty {
        id,
        property: CueProperty::Kind { value: c.kind },
    };
    let footprint = command.footprint();
    ws.apply(command).unwrap();
    cat.sync_cues(&ws, &footprint);
    assert_eq!(cat.preload_reserved(), 0);
    settle(&mut cat, &rx, &mut got);
    assert!(
        matches!(cat.media(&file), Some((_, Media::Path(_)))),
        "the late answer did not bring the samples back"
    );
    assert_eq!(cat.preload_reserved(), 0);
}

// ----- slice-plan checks followed from the catalog's notes (M4.12c) -----

use crate::control::checks::PlanChecks;
use openlab_core::SliceMarker;
use std::num::NonZeroU32;

const PLAN_RATE: NonZeroU32 = NonZeroU32::new(48_000).unwrap();

/// New trims, markers and counts for an audio cue (its file kept): a start past the end of
/// a 12.5 ms file fails, the rest plays (a marker past a short file cuts nothing).
fn plan_edit(ws: &Workspace, rng: &mut Rng) -> Option<Command> {
    let audio: Vec<&Cue> = all_cues(ws)
        .into_iter()
        .filter_map(|(id, _)| ws.find_cue(id))
        .filter(|c| matches!(c.kind, CueKind::Audio { .. }))
        .collect();
    let cue = *audio.get(rng.below(audio.len()))?;
    let mut kind = cue.kind.clone();
    if let CueKind::Audio {
        start_time,
        slices,
        last_slice_play_count,
        ..
    } = &mut kind
    {
        *start_time = [0.0, 0.004, 0.02][rng.below(3)];
        *slices = match rng.below(3) {
            0 => vec![],
            n => vec![SliceMarker {
                time: 0.3,
                play_count: u32::try_from(n).unwrap(),
            }],
        };
        *last_slice_play_count = [1, 2][rng.below(2)];
    }
    Some(Command::SetCueProperty {
        id: cue.id,
        property: CueProperty::Kind { value: kind },
    })
}

/// Plan checks told only what the catalog noted (the cues of each edit, the files whose
/// probe changed) answer exactly what a walk of every audio cue answers, after any edits,
/// undos and redos (groups removed and added again under their ids included), probe
/// answers, re-probes of files that changed length, and checks with nothing new; and they
/// look at what changed, not at the show.
#[test]
fn plan_checks_following_changes_equal_a_full_walk() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<String> = (0..5)
        .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
        .collect();
    let (base, template) = template();
    let (mut warned, mut cleared, mut reprobed) = (0, 0, 0);
    for seed in 1..=sweep_seeds(30) {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (mut cat, rx) = catalog(900);
        let mut got = 0;
        let (mut ws, mut history, mut next) = (base.clone(), History::new(), 20_000u128);
        cat.sync(&ws);
        let mut checks = PlanChecks::default();
        let mut last: Vec<(CueId, String)> = Vec::new();
        for step in 0..80 {
            let footprint = match rng.below(12) {
                0 => history
                    .peek_undo()
                    .map(openlab_core::Command::footprint)
                    .filter(|_| history.undo(&mut ws).unwrap_or(false)),
                1 => history
                    .peek_redo()
                    .map(openlab_core::Command::footprint)
                    .filter(|_| history.redo(&mut ws).unwrap_or(false)),
                2..=4 => plan_edit(&ws, &mut rng).and_then(|command| {
                    let footprint = command.footprint();
                    history.execute(&mut ws, command).ok().map(|_| footprint)
                }),
                5 => {
                    // A file changes length and is probed again.
                    let n = rng.below(files.len());
                    let frames = [100, 8000][rng.below(2)];
                    write_wav_frames(dir.path(), &format!("f{n}.wav"), frames);
                    cat.reprobe(&files[n]);
                    reprobed += 1;
                    None
                }
                6 => None,
                _ => {
                    let command = random_command(&ws, &mut rng, &template, &files, &mut next);
                    let footprint = command.footprint();
                    history.execute(&mut ws, command).ok().map(|_| footprint)
                }
            };
            if let Some(footprint) = &footprint {
                cat.sync_cues(&ws, footprint);
            }
            if rng.below(2) == 0 {
                settle(&mut cat, &rx, &mut got);
            }
            let got_now = checks.check(&ws, &mut cat, PLAN_RATE);
            let reference = PlanChecks::default().check_all(&ws, &cat, PLAN_RATE);
            assert_eq!(got_now, reference, "seed {seed} step {step}");
            warned += got_now.iter().filter(|w| !last.contains(w)).count();
            cleared += last.iter().filter(|w| !got_now.contains(w)).count();
            last = got_now;
        }
        // Only the first check (a newly followed workspace) walked the show.
        assert_eq!(checks.walks(), 1, "seed {seed}");
    }
    assert!(warned > 50, "warnings reached: {warned}");
    assert!(cleared > 30, "warnings cleared: {cleared}");
    assert!(reprobed > 50, "re-probes: {reprobed}");
}

/// The cost pin: in a show of 500 audio cues, an edit of one cue looks at that cue, a probe
/// answer for one file at the cues on it, and a check with nothing new at none.
#[test]
fn plan_checks_look_at_the_edit_not_the_show() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<String> = (0..5)
        .map(|n| write_wav(dir.path(), &format!("f{n}.wav")))
        .collect();
    let (mut ws, template) = template();
    let mut cues = Vec::new();
    for n in 0..500u128 {
        let mut c = template.clone();
        c.id = CueId(uuid::Uuid::from_u128(30_000 + n));
        c.number = None;
        set_media(&mut c, &files[usize::try_from(n % 5).unwrap()], false);
        cues.push(c);
    }
    let first = cues[0].id;
    ws.cue_lists[0].cues = cues;
    let (mut cat, rx) = catalog(0);
    let mut got = 0;
    cat.sync(&ws);
    settle(&mut cat, &rx, &mut got);
    let mut checks = PlanChecks::default();
    checks.check(&ws, &mut cat, PLAN_RATE);
    let examined = checks.examined();
    checks.check(&ws, &mut cat, PLAN_RATE);
    assert_eq!(
        checks.examined(),
        examined,
        "nothing new: nothing looked at"
    );

    let mut rng = Rng(7);
    let mut command = plan_edit(&ws, &mut rng).unwrap();
    if let Command::SetCueProperty { id, .. } = &mut command {
        *id = first;
    }
    let footprint = command.footprint();
    ws.apply(command).unwrap();
    cat.sync_cues(&ws, &footprint);
    let builds = checks.builds();
    checks.check(&ws, &mut cat, PLAN_RATE);
    assert_eq!(checks.examined(), examined + 1, "an edit: its cue");
    assert_eq!(checks.builds(), builds + 1);

    // One file probed again: its 100 cues.
    cat.reprobe(&files[1]);
    settle(&mut cat, &rx, &mut got);
    let examined = checks.examined();
    checks.check(&ws, &mut cat, PLAN_RATE);
    assert_eq!(
        checks.examined(),
        examined + 100,
        "a probe: the cues on its file"
    );
    assert_eq!(
        checks.check(&ws, &mut cat, PLAN_RATE),
        PlanChecks::default().check_all(&ws, &cat, PLAN_RATE)
    );
    assert_eq!(checks.walks(), 1);
}

mod volume;
