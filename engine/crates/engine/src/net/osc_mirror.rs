// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC side's copy of the workspace (docs/osc.md, "Updates"). It is built from a `snapshot`
//! and kept current from the `model_changed`, `runtime` and `document` broadcasts with the
//! reference client logic (`Workspace::apply_changes`), so answering an OSC message never asks
//! the control thread anything. A gap in the numbering, or an edit that does not apply, means
//! the copy cannot be trusted: the caller asks for a new snapshot.
//!
//! Besides the model it holds what the OSC queries need that the model does not say: each cue's
//! run state, the playheads, and which cue list is the active one.

use std::collections::HashMap;
use std::ops::ControlFlow;

use openlab_core::protocol::{
    Delete, DocumentState, RuntimeChange, Snapshot, Upsert, WireCueState,
};
use openlab_core::{Cue, CueId, CueKind, CueList, CueListId, Workspace, WorkspaceId};
use serde_json::{Value, json};

use crate::osc::{CueField, OscShow, Target, upper};

/// Most cues one change reports on one by one. Past it one `__root__` update ("ask again") is
/// sent instead, so pasting 500 cues does not become 2000 datagrams for every subscriber.
pub const MAX_CUE_UPDATES: usize = 32;

/// Deepest group nesting `/cueLists` and the cue objects write out; a group below it is written
/// with no children. Keeps a hostile show from overflowing the stack.
const MAX_OBJECT_DEPTH: usize = 16;

/// What an update message is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateId {
    Cue(CueId),
    List(CueListId),
    /// QLab's `__root__`: the top level, the list of cue lists.
    Root,
}

/// What changed, for the update messages (NET-05): QLab 5's three kinds, as captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Update {
    /// `/update/workspace/{ws}/cue_id/{id}`: this cue (or cue list, or the root) changed; ask.
    Id(UpdateId),
    /// `.../cueList/{list}/playbackPosition`: the playhead moved (`None`: past the end).
    Playhead { list: CueListId, cue: Option<CueId> },
    /// `.../dashboard`: the playhead moved, a cue started, or the cue tree changed shape.
    Dashboard,
}

/// How a broadcast met the copy.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Already contained (at or below the copy's revision or seq): ignored.
    Stale,
    /// Applied.
    Updates(Vec<Update>),
    /// A message was missed, or an edit did not apply: a new snapshot is needed.
    Resync,
}

#[derive(Debug)]
pub struct Mirror {
    workspace: Workspace,
    revision: u64,
    runtime_seq: u64,
    document: DocumentState,
    playheads: HashMap<CueListId, Option<CueId>>,
    /// Cues that are not idle.
    states: HashMap<CueId, WireCueState>,
    /// The list `/go` and `/playhead` act on: the one whose playhead moved last (QLab: the list
    /// on display, which selecting a cue in another list switches). `None` means the first.
    active: Option<CueListId>,
}

/// Where a cue sits: its list, the groups around it (outermost first) and its place among its
/// siblings.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Location {
    list: CueListId,
    groups: Vec<CueId>,
    index: usize,
}

impl Mirror {
    pub fn from_snapshot(snapshot: Snapshot) -> Self {
        Self {
            playheads: snapshot.playheads.iter().map(|p| (p.list, p.cue)).collect(),
            states: snapshot
                .cue_states
                .iter()
                .map(|entry| (entry.cue, entry.state))
                .collect(),
            workspace: snapshot.workspace,
            revision: snapshot.revision,
            runtime_seq: snapshot.runtime_seq,
            document: snapshot.document,
            active: None,
        }
    }

    /// The model revision the copy is at.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The last `runtime` seq folded in.
    pub fn runtime_seq(&self) -> u64 {
        self.runtime_seq
    }

    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace.id
    }

    pub fn passcode(&self) -> Option<&str> {
        self.workspace.settings.osc_passcode.as_deref()
    }

    /// Takes the active list over from the copy this one replaces, if it is the same workspace
    /// and the list is still there.
    pub fn inherit_active(&mut self, old: &Mirror) {
        if old.workspace.id == self.workspace.id
            && let Some(list) = old.active
        {
            self.set_active(list);
        }
    }

    /// Makes `list` the active one, ahead of the playhead broadcast that will say the same: a
    /// `/go` right after `/playhead/20` (a cue in another list) must act on that list.
    pub fn set_active(&mut self, list: CueListId) {
        if self.workspace.cue_lists.iter().any(|l| l.id == list) {
            self.active = Some(list);
        }
    }

    /// What a replacement of this copy by `new` means for subscribers: the root changed if the
    /// workspace differs, and a playhead update for each list whose playhead moved.
    pub fn updates_to(&self, new: &Mirror) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut dashboard = false;
        if self.workspace != new.workspace {
            updates.push(Update::Id(UpdateId::Root));
            dashboard = true;
        }
        for list in &new.workspace.cue_lists {
            let before = self.playheads.get(&list.id).copied().flatten();
            let after = new.playheads.get(&list.id).copied().flatten();
            if before != after {
                updates.push(Update::Playhead {
                    list: list.id,
                    cue: after,
                });
                dashboard = true;
            }
        }
        if dashboard || self.states != new.states {
            updates.push(Update::Dashboard);
        }
        updates
    }

    pub fn model_changed(
        &mut self,
        revision: u64,
        upserts: &[Upsert],
        deletes: &[Delete],
    ) -> Applied {
        if revision <= self.revision {
            return Applied::Stale;
        }
        if revision != self.revision + 1 {
            return Applied::Resync;
        }
        let mut touched: Vec<CueId> = Vec::new();
        let mut root = false;
        for delete in deletes {
            match delete {
                Delete::Cue { id } => push_unique(&mut touched, *id),
                Delete::CueList { .. } => root = true,
                Delete::Patch { .. } | Delete::NetworkPatch { .. } => {}
            }
        }
        for upsert in upserts {
            match upsert {
                Upsert::Cue { cue, .. } => push_unique(&mut touched, cue.id),
                Upsert::CueList { .. } => root = true,
                Upsert::Patch { .. } | Upsert::NetworkPatch { .. } | Upsert::Settings { .. } => {}
            }
        }
        // Where each cue was, judged before the edit (a deleted cue is gone afterwards).
        let overflow = touched.len() > MAX_CUE_UPDATES;
        let before: Vec<Option<Location>> = if overflow {
            Vec::new()
        } else {
            touched
                .iter()
                .map(|id| locate(&self.workspace, *id))
                .collect()
        };
        if self.workspace.apply_changes(upserts, deletes).is_err() {
            return Applied::Resync;
        }
        self.revision = revision;
        self.forget_missing_lists();
        let mut updates = Updates {
            root,
            ..Updates::default()
        };
        if overflow {
            updates.root = true;
            updates.dashboard = true;
        } else {
            for (id, was) in touched.iter().zip(before) {
                let now = locate(&self.workspace, *id);
                // A cue that stayed where it was (or was never there) changed its properties
                // only; one that was added, removed or moved changed the shape of its list and of
                // the groups around it.
                updates.cue(*id);
                if was == now {
                    continue;
                }
                for place in [was, now].into_iter().flatten() {
                    updates.parents(&place);
                }
                updates.root = true;
                updates.dashboard = true;
            }
        }
        Applied::Updates(updates.finish())
    }

    pub fn runtime(&mut self, seq: u64, changes: &[RuntimeChange]) -> Applied {
        if seq <= self.runtime_seq {
            return Applied::Stale;
        }
        if seq != self.runtime_seq + 1 {
            return Applied::Resync;
        }
        self.runtime_seq = seq;
        let mut updates = Updates::default();
        let mut playheads = Vec::new();
        let mut states: Vec<CueId> = Vec::new();
        for change in changes {
            match change {
                RuntimeChange::CueState { cue, state, .. } => {
                    let was = self.states.get(cue).copied();
                    if *state == WireCueState::Idle {
                        self.states.remove(cue);
                    } else {
                        self.states.insert(*cue, *state);
                    }
                    // A cue that went from idle to active is a start: the dashboard shows it.
                    if !is_active(was) && is_active(Some(*state)) {
                        updates.dashboard = true;
                    }
                    push_unique(&mut states, *cue);
                }
                // Only a move is news: the insert says what was there.
                RuntimeChange::Playhead { list, cue }
                    if self.playheads.insert(*list, *cue) != Some(*cue) =>
                {
                    self.active = Some(*list);
                    playheads.push(Update::Playhead {
                        list: *list,
                        cue: *cue,
                    });
                    updates.dashboard = true;
                }
                _ => {}
            }
        }
        if states.len() > MAX_CUE_UPDATES {
            updates.root = true;
        } else {
            for cue in states {
                updates.cue(cue);
                if let Some(place) = locate(&self.workspace, cue) {
                    updates.parents(&place);
                    updates.root = true;
                }
            }
        }
        Applied::Updates(updates.finish_with(playheads))
    }

    /// A save-as or another file: the workspace may be a different one, which only a snapshot
    /// tells (a `document` message does not say what was opened).
    pub fn document(&mut self, document: &DocumentState) -> Applied {
        if document.path == self.document.path {
            self.document = document.clone();
            Applied::Updates(Vec::new())
        } else {
            Applied::Resync
        }
    }

    /// Drops the active list if an edit removed it.
    fn forget_missing_lists(&mut self) {
        if let Some(active) = self.active
            && !self.workspace.cue_lists.iter().any(|l| l.id == active)
        {
            self.active = None;
        }
    }

    fn running_state(&self, id: CueId) -> Option<WireCueState> {
        self.states.get(&id).copied()
    }
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

/// Running: waiting, playing, fading or stopping (not paused, loaded or broken).
fn is_running(state: Option<WireCueState>) -> bool {
    matches!(
        state,
        Some(
            WireCueState::PreWaiting
                | WireCueState::Running
                | WireCueState::PostWaiting
                | WireCueState::Fading
                | WireCueState::Stopping
        )
    )
}

/// Running or paused.
fn is_active(state: Option<WireCueState>) -> bool {
    is_running(state) || state == Some(WireCueState::Paused)
}

/// Where `id` is, or `None`. Iterative: a hostile show cannot overflow the stack.
fn locate(workspace: &Workspace, id: CueId) -> Option<Location> {
    for list in &workspace.cue_lists {
        // Each level: the cues still to visit, the group they are in, and how many were taken.
        let mut stack: Vec<(std::slice::Iter<'_, Cue>, Option<CueId>, usize)> =
            vec![(list.cues.iter(), None, 0)];
        while let Some((level, _, taken)) = stack.last_mut() {
            let Some(cue) = level.next() else {
                stack.pop();
                continue;
            };
            let index = *taken;
            *taken += 1;
            if cue.id == id {
                return Some(Location {
                    list: list.id,
                    groups: stack.iter().filter_map(|(_, g, _)| *g).collect(),
                    index,
                });
            }
            if let CueKind::Group { children, .. } = &cue.kind {
                stack.push((children.iter(), Some(cue.id), 0));
            }
        }
    }
    None
}

/// Collects the updates of one change, in order, without repeats: cue updates, then the root,
/// then playheads, then the dashboard.
#[derive(Default)]
struct Updates {
    ids: Vec<UpdateId>,
    root: bool,
    dashboard: bool,
}

impl Updates {
    fn cue(&mut self, id: CueId) {
        push_unique(&mut self.ids, UpdateId::Cue(id));
    }

    /// The groups around a cue (nearest first) and its list changed too.
    fn parents(&mut self, place: &Location) {
        for group in place.groups.iter().rev() {
            push_unique(&mut self.ids, UpdateId::Cue(*group));
        }
        push_unique(&mut self.ids, UpdateId::List(place.list));
    }

    fn finish(self) -> Vec<Update> {
        self.finish_with(Vec::new())
    }

    fn finish_with(self, playheads: Vec<Update>) -> Vec<Update> {
        let mut out: Vec<Update> = self.ids.into_iter().map(Update::Id).collect();
        if self.root {
            out.push(Update::Id(UpdateId::Root));
        }
        out.extend(playheads);
        if self.dashboard {
            out.push(Update::Dashboard);
        }
        out
    }
}

/// The dispatcher's view of the show: the copy, read-only.
pub struct MirrorShow<'a>(pub &'a Mirror);

impl MirrorShow<'_> {
    /// First cue, depth-first, for which `pick` says yes. Iterative: a hostile show cannot
    /// overflow the stack.
    fn find(&self, pick: impl Fn(&Cue) -> bool) -> Option<CueId> {
        let mut found = None;
        self.walk(|cue, _| {
            if pick(cue) {
                found = Some(cue.id);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        found
    }

    /// Calls `visit` with every cue and its list, in workspace order (each list's cues depth
    /// first, a group before its children), until it breaks. Iterative.
    fn walk(&self, mut visit: impl FnMut(&Cue, &CueList) -> ControlFlow<()>) {
        for list in &self.0.workspace.cue_lists {
            let mut stack: Vec<&[Cue]> = vec![list.cues.as_slice()];
            while let Some(cues) = stack.pop() {
                let Some((cue, rest)) = cues.split_first() else {
                    continue;
                };
                if visit(cue, list).is_break() {
                    return;
                }
                // The children come before the siblings after the group.
                stack.push(rest);
                stack.push(cue.children());
            }
        }
    }

    fn cue_json(&self, cue: &Cue, depth: usize) -> Value {
        let color = serde_json::to_value(cue.color).unwrap_or_else(|_| json!("none"));
        let children: Vec<Value> = if depth < MAX_OBJECT_DEPTH {
            cue.children()
                .iter()
                .map(|child| self.cue_json(child, depth + 1))
                .collect()
        } else {
            Vec::new()
        };
        json!({
            "number": cue.number.as_deref().unwrap_or(""),
            "uniqueID": upper(cue.id),
            "cues": children,
            "colorName/live": color,
            "flagged": cue.flagged,
            "listName": cue.name,
            "type": kind_name(&cue.kind),
            "colorName": color,
            "name": cue.name,
            "armed": cue.armed,
        })
    }
}

/// The cue's type as the cue objects name it. The names for kinds other than Audio, Group and
/// Cue List are ours (QLab's were not measured).
fn kind_name(kind: &CueKind) -> &'static str {
    match kind {
        CueKind::Audio { .. } => "Audio",
        CueKind::Fade { .. } => "Fade",
        CueKind::Group { .. } => "Group",
        CueKind::Start { .. } => "Start",
        CueKind::Stop { .. } => "Stop",
        CueKind::Pause { .. } => "Pause",
        CueKind::Load { .. } => "Load",
        CueKind::Reset { .. } => "Reset",
        CueKind::Devamp { .. } => "Devamp",
        CueKind::GoTo { .. } => "GoTo",
        CueKind::Arm { .. } => "Arm",
        CueKind::Disarm { .. } => "Disarm",
        CueKind::Wait { .. } => "Wait",
        CueKind::Memo => "Memo",
        CueKind::Network { .. } => "Network",
    }
}

impl OscShow for MirrorShow<'_> {
    fn workspace_id(&self) -> WorkspaceId {
        self.0.workspace.id
    }

    fn workspace_name(&self) -> &str {
        &self.0.workspace.name
    }

    fn passcode(&self) -> Option<&str> {
        self.0.passcode()
    }

    /// The list whose playhead moved last, else the first (the main list).
    fn active_list(&self) -> Option<CueListId> {
        self.0
            .active
            .filter(|id| self.0.workspace.cue_lists.iter().any(|l| l.id == *id))
            .or_else(|| self.0.workspace.cue_lists.first().map(|l| l.id))
    }

    fn cue_by_number(&self, number: &str) -> Option<CueId> {
        self.find(|c| c.number.as_deref() == Some(number))
    }

    fn cue_exists(&self, id: CueId) -> bool {
        self.0.workspace.find_cue(id).is_some()
    }

    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value> {
        let cue = self.0.workspace.find_cue(id)?;
        let state = self.0.running_state(id);
        Some(match field {
            CueField::Name => json!(cue.name),
            CueField::Number => json!(cue.number.as_deref().unwrap_or("")),
            CueField::Notes => json!(cue.notes),
            CueField::PreWait => json!(cue.pre_wait),
            CueField::PostWait => json!(cue.post_wait),
            CueField::Armed => json!(cue.armed),
            CueField::Flagged => json!(cue.flagged),
            CueField::UniqueId => json!(upper(cue.id)),
            CueField::IsLoaded => json!(state == Some(WireCueState::Loaded)),
            CueField::IsRunning => json!(is_running(state)),
            CueField::IsPaused => json!(state == Some(WireCueState::Paused)),
            CueField::IsBroken => json!(state == Some(WireCueState::Broken)),
        })
    }

    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId> {
        self.find(|c| c.triggers.osc.as_deref() == Some(address))
    }

    fn top_level_list(&self, cue: CueId) -> Option<CueListId> {
        self.0
            .workspace
            .cue_lists
            .iter()
            .find(|l| l.cues.iter().any(|c| c.id == cue))
            .map(|l| l.id)
    }

    fn playhead(&self, list: CueListId) -> Option<CueId> {
        self.0.playheads.get(&list).copied().flatten()
    }

    fn list_name(&self, list: CueListId) -> Option<String> {
        self.0
            .workspace
            .cue_lists
            .iter()
            .find(|l| l.id == list)
            .map(|l| l.name.clone())
    }

    fn running_cues(&self, include_paused: bool) -> Vec<CueId> {
        let mut found = Vec::new();
        self.walk(|cue, _| {
            let state = self.0.running_state(cue.id);
            if is_running(state) || (include_paused && state == Some(WireCueState::Paused)) {
                found.push(cue.id);
            }
            ControlFlow::Continue(())
        });
        found
    }

    fn visit_targets(&self, visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {
        for list in &self.0.workspace.cue_lists {
            if visit(Target::List(list.id), "").is_break() {
                return;
            }
            let mut stack: Vec<&[Cue]> = vec![list.cues.as_slice()];
            while let Some(cues) = stack.pop() {
                let Some((cue, rest)) = cues.split_first() else {
                    continue;
                };
                if visit(Target::Cue(cue.id), cue.number.as_deref().unwrap_or("")).is_break() {
                    return;
                }
                stack.push(rest);
                stack.push(cue.children());
            }
        }
    }

    fn cue_object(&self, id: CueId) -> Option<Value> {
        self.0.workspace.find_cue(id).map(|c| self.cue_json(c, 0))
    }

    fn cue_lists_object(&self) -> Value {
        Value::Array(
            self.0
                .workspace
                .cue_lists
                .iter()
                .map(|list| {
                    json!({
                        "number": "",
                        "uniqueID": upper(list.id),
                        "cues": list.cues.iter().map(|c| self.cue_json(c, 1)).collect::<Vec<_>>(),
                        "colorName/live": "none",
                        "flagged": false,
                        "listName": list.name,
                        "type": "Cue List",
                        "colorName": "none",
                        "name": list.name,
                        "armed": true,
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use openlab_core::protocol::{CueStateEntry, PlayheadEntry};
    use openlab_core::{
        ContinueMode, CueColor, GroupMode, SecondTrigger, TransportHotkeys, Triggers,
        WorkspaceSettings,
    };
    use uuid::Uuid;

    use super::*;

    fn cue_id(n: u128) -> CueId {
        CueId(Uuid::from_u128(n))
    }

    fn list_id() -> CueListId {
        CueListId(Uuid::from_u128(900))
    }

    fn cue(n: u128) -> Cue {
        Cue {
            id: cue_id(n),
            number: Some(n.to_string()),
            name: format!("cue {n}"),
            notes: String::new(),
            color: CueColor::None,
            flagged: false,
            armed: true,
            pre_wait: 0.0,
            post_wait: 0.0,
            continue_mode: ContinueMode::DoNotContinue,
            triggers: Triggers {
                hotkey: None,
                osc: None,
            },
            second_trigger: SecondTrigger::Nothing,
            second_trigger_on_release: false,
            kind: CueKind::Memo,
        }
    }

    fn group(n: u128, children: Vec<Cue>) -> Cue {
        let mut c = cue(n);
        c.kind = CueKind::Group {
            mode: GroupMode::Timeline,
            children,
        };
        c
    }

    fn mirror(cues: u128) -> Mirror {
        let workspace = Workspace {
            id: WorkspaceId(Uuid::from_u128(1)),
            name: "t".into(),
            settings: WorkspaceSettings {
                min_go_interval: 0.0,
                panic_duration: 0.1,
                autosave_interval: 30.0,
                osc_passcode: None,
                master_db: 0.0,
                hotkeys: TransportHotkeys::default(),
                auto_number_start: 1.0,
                auto_number_increment: 1.0,
            },
            patches: vec![],
            network_patches: vec![],
            cue_lists: vec![CueList {
                id: list_id(),
                name: "Main".into(),
                cues: (1..=cues).map(cue).collect(),
            }],
        };
        Mirror::from_snapshot(Snapshot {
            revision: 0,
            runtime_seq: 0,
            document: DocumentState {
                path: None,
                dirty: false,
                can_undo: false,
                can_redo: false,
            },
            workspace,
            cue_states: vec![],
            problems: vec![],
            group_warnings: vec![],
            playheads: vec![PlayheadEntry {
                list: list_id(),
                cue: Some(cue_id(1)),
            }],
            previewing: vec![],
            warnings: vec![],
        })
    }

    fn upsert(n: u128, index: u64) -> Upsert {
        Upsert::Cue {
            list: list_id(),
            parent: None,
            index,
            cue: Box::new(cue(n)),
        }
    }

    fn upserts(range: std::ops::RangeInclusive<u128>) -> Vec<Upsert> {
        range
            .map(|i| upsert(i, u64::try_from(i - 1).unwrap_or(0)))
            .collect()
    }

    fn id(n: u128) -> Update {
        Update::Id(UpdateId::Cue(cue_id(n)))
    }

    fn list() -> Update {
        Update::Id(UpdateId::List(list_id()))
    }

    fn root() -> Update {
        Update::Id(UpdateId::Root)
    }

    fn state(n: u128, state: WireCueState) -> RuntimeChange {
        RuntimeChange::CueState {
            cue: cue_id(n),
            state,
            reason: None,
        }
    }

    #[test]
    fn revisions_are_stale_applied_or_a_gap() {
        let mut m = mirror(2);
        assert_eq!(m.model_changed(0, &[], &[]), Applied::Stale);
        assert_eq!(m.model_changed(2, &[], &[]), Applied::Resync);
        assert_eq!(m.model_changed(1, &[], &[]), Applied::Updates(vec![]));
        assert_eq!(m.model_changed(1, &[], &[]), Applied::Stale);
        assert_eq!(m.model_changed(2, &[], &[]), Applied::Updates(vec![]));
    }

    #[test]
    fn an_edit_is_one_cue_update_and_a_change_of_shape_reaches_the_list_and_the_root() {
        let mut m = mirror(2);
        // The cue is there and stays: only its properties changed.
        assert_eq!(
            m.model_changed(1, &[upsert(1, 0)], &[]),
            Applied::Updates(vec![id(1)])
        );
        // A cue that was not there: the cue, its list, the root, the dashboard.
        assert_eq!(
            m.model_changed(2, &[upsert(3, 2)], &[]),
            Applied::Updates(vec![id(3), list(), root(), Update::Dashboard])
        );
        // A delete of one that is there; of one that is not, nothing about the shape.
        assert_eq!(
            m.model_changed(3, &[], &[Delete::Cue { id: cue_id(3) }]),
            Applied::Updates(vec![id(3), list(), root(), Update::Dashboard])
        );
        assert_eq!(
            m.model_changed(4, &[], &[Delete::Cue { id: cue_id(77) }]),
            Applied::Updates(vec![id(77)])
        );
        // A move is a delete and an upsert of the same cue: one cue update, and the shape.
        assert_eq!(
            m.model_changed(5, &[upsert(1, 1)], &[Delete::Cue { id: cue_id(1) }]),
            Applied::Updates(vec![id(1), list(), root(), Update::Dashboard])
        );
        // Patches and settings have no QLab counterpart: nothing is sent.
        let patch = Delete::Patch {
            id: openlab_core::PatchId(Uuid::from_u128(5)),
        };
        assert_eq!(m.model_changed(6, &[], &[patch]), Applied::Updates(vec![]));
        // A cue list that went is a change of the root.
        assert_eq!(
            m.model_changed(
                7,
                &[],
                &[Delete::CueList {
                    id: CueListId(Uuid::from_u128(4242))
                }]
            ),
            Applied::Updates(vec![root()])
        );
    }

    #[test]
    fn a_cue_inside_groups_reports_every_group_around_it_nearest_first() {
        let mut m = mirror(0);
        m.workspace.cue_lists[0].cues = vec![group(10, vec![group(11, vec![cue(12)])])];
        let new = Upsert::Cue {
            list: list_id(),
            parent: Some(cue_id(11)),
            index: 1,
            cue: Box::new(cue(13)),
        };
        assert_eq!(
            m.model_changed(1, &[new], &[]),
            Applied::Updates(vec![
                id(13),
                id(11),
                id(10),
                list(),
                root(),
                Update::Dashboard
            ])
        );
        let Applied::Updates(running) = m.runtime(1, &[state(12, WireCueState::Running)]) else {
            panic!("not applied")
        };
        assert_eq!(
            running,
            vec![id(12), id(11), id(10), list(), root(), Update::Dashboard]
        );
    }

    #[test]
    fn more_than_the_cap_of_cue_updates_become_one_root_update() {
        let n = MAX_CUE_UPDATES as u128;
        let mut m = mirror(n + 8);
        let Applied::Updates(updates) = m.model_changed(1, &upserts(1..=n), &[]) else {
            panic!("not applied")
        };
        // The edits of existing cues are one update each: no shape change, no root.
        assert_eq!(updates.len(), MAX_CUE_UPDATES);
        assert_eq!(
            m.model_changed(2, &upserts(1..=n + 1), &[]),
            Applied::Updates(vec![root(), Update::Dashboard])
        );
        // Many state changes at once likewise.
        let changes: Vec<RuntimeChange> = (1..=n + 1)
            .map(|i| state(i, WireCueState::Running))
            .collect();
        assert_eq!(
            m.runtime(1, &changes),
            Applied::Updates(vec![root(), Update::Dashboard])
        );
    }

    #[test]
    fn an_edit_that_does_not_apply_is_a_resync_and_the_revision_stays() {
        let mut m = mirror(1);
        let bad = Upsert::Cue {
            list: CueListId(Uuid::from_u128(12345)),
            parent: None,
            index: 0,
            cue: Box::new(cue(9)),
        };
        assert_eq!(m.model_changed(1, &[bad], &[]), Applied::Resync);
        assert_eq!(m.revision, 0);
    }

    #[test]
    fn runtime_seq_and_playheads_send_a_playhead_update_and_the_dashboard() {
        let mut m = mirror(2);
        let head = |cue| RuntimeChange::Playhead {
            list: list_id(),
            cue,
        };
        let moved = |cue| {
            Applied::Updates(vec![
                Update::Playhead {
                    list: list_id(),
                    cue,
                },
                Update::Dashboard,
            ])
        };
        assert_eq!(m.runtime(0, &[]), Applied::Stale);
        assert_eq!(m.runtime(2, &[]), Applied::Resync);
        // The playhead was on cue 1 in the snapshot: staying there says nothing.
        assert_eq!(
            m.runtime(1, &[head(Some(cue_id(1)))]),
            Applied::Updates(vec![])
        );
        assert_eq!(
            m.runtime(2, &[head(Some(cue_id(2)))]),
            moved(Some(cue_id(2)))
        );
        assert_eq!(
            m.runtime(3, &[head(Some(cue_id(2)))]),
            Applied::Updates(vec![])
        );
        // Past the end: a playhead update with no cue.
        assert_eq!(m.runtime(4, &[head(None)]), moved(None));
    }

    #[test]
    fn a_start_sends_the_dashboard_but_pause_resume_and_finish_do_not() {
        let mut m = mirror(2);
        let cue_updates = vec![id(1), list(), root()];
        let with_dashboard = [cue_updates.clone(), vec![Update::Dashboard]].concat();
        let step = |m: &mut Mirror, seq, to| m.runtime(seq, &[state(1, to)]);
        assert_eq!(
            step(&mut m, 1, WireCueState::Running),
            Applied::Updates(with_dashboard.clone())
        );
        assert_eq!(
            step(&mut m, 2, WireCueState::Paused),
            Applied::Updates(cue_updates.clone())
        );
        assert_eq!(
            step(&mut m, 3, WireCueState::Running),
            Applied::Updates(cue_updates.clone())
        );
        assert_eq!(
            step(&mut m, 4, WireCueState::Idle),
            Applied::Updates(cue_updates.clone())
        );
        // A pre-wait is a start too; a load is not.
        assert_eq!(
            step(&mut m, 5, WireCueState::Loaded),
            Applied::Updates(cue_updates)
        );
        assert_eq!(
            step(&mut m, 6, WireCueState::PreWaiting),
            Applied::Updates(with_dashboard)
        );
    }

    #[test]
    fn cue_states_and_playheads_in_one_change_come_cues_first_and_the_dashboard_last() {
        let mut m = mirror(2);
        let head = RuntimeChange::Playhead {
            list: list_id(),
            cue: Some(cue_id(2)),
        };
        assert_eq!(
            m.runtime(
                1,
                &[
                    head,
                    state(1, WireCueState::Running),
                    state(1, WireCueState::Running)
                ]
            ),
            Applied::Updates(vec![
                id(1),
                list(),
                root(),
                Update::Playhead {
                    list: list_id(),
                    cue: Some(cue_id(2))
                },
                Update::Dashboard
            ])
        );
    }

    #[test]
    fn a_document_with_another_path_is_a_resync() {
        let mut m = mirror(1);
        let doc = |path: Option<&str>, dirty| DocumentState {
            path: path.map(Into::into),
            dirty,
            can_undo: false,
            can_redo: false,
        };
        assert_eq!(m.document(&doc(None, true)), Applied::Updates(vec![]));
        assert_eq!(m.document(&doc(Some("/a"), false)), Applied::Resync);
    }

    #[test]
    fn a_replacement_reports_the_root_and_the_moved_playheads() {
        let old = mirror(2);
        assert!(old.updates_to(&mirror(2)).is_empty());
        let mut changed = mirror(3);
        changed.playheads.insert(list_id(), Some(cue_id(3)));
        assert_eq!(
            old.updates_to(&changed),
            vec![
                root(),
                Update::Playhead {
                    list: list_id(),
                    cue: Some(cue_id(3))
                },
                Update::Dashboard
            ]
        );
    }

    #[test]
    fn lookups_walk_groups_in_tree_order() {
        let mut m = mirror(2);
        let mut child = cue(11);
        child.number = Some("2".into());
        child.triggers.osc = Some("/x".into());
        m.workspace.cue_lists[0]
            .cues
            .insert(0, group(10, vec![child]));
        let show = MirrorShow(&m);
        // The group's child comes before the later cue numbered "2".
        assert_eq!(show.cue_by_number("2"), Some(cue_id(11)));
        assert_eq!(show.cue_by_number("1"), Some(cue_id(1)));
        assert_eq!(show.cue_by_number("nope"), None);
        assert_eq!(show.cue_by_osc_trigger("/x"), Some(cue_id(11)));
        assert!(show.cue_exists(cue_id(11)));
        assert_eq!(show.active_list(), Some(list_id()));
        // Only a cue directly in a list can have the playhead.
        assert_eq!(show.top_level_list(cue_id(10)), Some(list_id()));
        assert_eq!(show.top_level_list(cue_id(11)), None);
    }

    fn two_lists() -> Mirror {
        let mut m = mirror(2);
        m.workspace.cue_lists.push(CueList {
            id: CueListId(Uuid::from_u128(901)),
            name: "Second".into(),
            cues: vec![cue(20)],
        });
        m
    }

    #[test]
    fn the_active_list_is_the_one_whose_playhead_moved_last() {
        let mut m = two_lists();
        let second = CueListId(Uuid::from_u128(901));
        assert_eq!(MirrorShow(&m).active_list(), Some(list_id()));
        m.runtime(
            1,
            &[RuntimeChange::Playhead {
                list: second,
                cue: Some(cue_id(20)),
            }],
        );
        assert_eq!(MirrorShow(&m).active_list(), Some(second));
        // Told ahead of the broadcast, for a GO that follows at once.
        m.set_active(list_id());
        assert_eq!(MirrorShow(&m).active_list(), Some(list_id()));
        m.set_active(CueListId(Uuid::from_u128(5)));
        assert_eq!(MirrorShow(&m).active_list(), Some(list_id()));
        // A list that is deleted is no longer active.
        m.set_active(second);
        m.model_changed(1, &[], &[Delete::CueList { id: second }]);
        assert_eq!(MirrorShow(&m).active_list(), Some(list_id()));
    }

    #[test]
    fn running_cues_follow_the_run_states_in_workspace_order() {
        let mut m = two_lists();
        m.runtime(
            1,
            &[
                state(20, WireCueState::Running),
                state(2, WireCueState::Paused),
                state(1, WireCueState::PostWaiting),
            ],
        );
        let show = MirrorShow(&m);
        assert_eq!(show.running_cues(false), vec![cue_id(1), cue_id(20)]);
        assert_eq!(
            show.running_cues(true),
            vec![cue_id(1), cue_id(2), cue_id(20)]
        );
        let flag = |n, field| show.cue_property(cue_id(n), field);
        assert_eq!(flag(1, CueField::IsRunning), Some(json!(true)));
        assert_eq!(flag(2, CueField::IsRunning), Some(json!(false)));
        assert_eq!(flag(2, CueField::IsPaused), Some(json!(true)));
        assert_eq!(flag(1, CueField::IsBroken), Some(json!(false)));
        assert_eq!(
            flag(1, CueField::UniqueId),
            Some(json!("00000000-0000-0000-0000-000000000001"))
        );
    }

    #[test]
    fn a_snapshots_cue_states_are_kept() {
        let base = mirror(1);
        let m = Mirror::from_snapshot(Snapshot {
            revision: 0,
            runtime_seq: 0,
            document: base.document.clone(),
            workspace: base.workspace.clone(),
            cue_states: vec![CueStateEntry {
                cue: cue_id(1),
                state: WireCueState::Broken,
                reason: None,
            }],
            problems: vec![],
            group_warnings: vec![],
            playheads: vec![],
            previewing: vec![],
            warnings: vec![],
        });
        let show = MirrorShow(&m);
        assert_eq!(
            show.cue_property(cue_id(1), CueField::IsBroken),
            Some(json!(true))
        );
    }

    #[test]
    fn cue_objects_and_cue_lists_have_the_captured_fields_and_nest_groups() {
        let mut m = two_lists();
        m.workspace.cue_lists[0].cues[0] = group(1, vec![cue(5)]);
        let show = MirrorShow(&m);
        let object = show.cue_object(cue_id(1)).unwrap();
        let keys: Vec<&str> = object
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "number",
            "uniqueID",
            "cues",
            "colorName/live",
            "flagged",
            "listName",
            "type",
            "colorName",
            "name",
            "armed",
        ] {
            assert!(keys.contains(&key), "missing {key}");
        }
        assert_eq!(object["type"], "Group");
        assert_eq!(object["cues"][0]["number"], "5");
        let lists = show.cue_lists_object();
        assert_eq!(lists.as_array().unwrap().len(), 2);
        assert_eq!(lists[0]["type"], "Cue List");
        assert_eq!(lists[0]["number"], "");
        assert_eq!(lists[0]["cues"][0]["cues"][0]["name"], "cue 5");
        assert_eq!(lists[1]["cues"][0]["number"], "20");
    }

    #[test]
    fn a_hostile_nesting_depth_is_cut_off_not_recursed() {
        let mut m = mirror(0);
        let mut deepest = cue(1000);
        for n in (1..1000).rev() {
            deepest = group(n, vec![deepest]);
        }
        m.workspace.cue_lists[0].cues = vec![deepest];
        let show = MirrorShow(&m);
        let object = show.cue_object(cue_id(1)).unwrap();
        let mut depth = 0;
        let mut at = &object;
        while let Some(child) = at["cues"].get(0) {
            depth += 1;
            at = child;
        }
        assert_eq!(depth, MAX_OBJECT_DEPTH);
        // Walks, lookups and locations are iterative.
        assert_eq!(show.cue_by_number("1000"), Some(cue_id(1000)));
        assert_eq!(
            locate(&m.workspace, cue_id(1000)).unwrap().groups.len(),
            999
        );
    }

    #[test]
    fn locate_gives_the_list_the_groups_and_the_index() {
        let mut m = mirror(2);
        m.workspace.cue_lists[0].cues = vec![cue(1), group(10, vec![cue(11), cue(12)])];
        assert_eq!(
            locate(&m.workspace, cue_id(12)),
            Some(Location {
                list: list_id(),
                groups: vec![cue_id(10)],
                index: 1
            })
        );
        assert_eq!(
            locate(&m.workspace, cue_id(10)),
            Some(Location {
                list: list_id(),
                groups: vec![],
                index: 1
            })
        );
        assert_eq!(locate(&m.workspace, cue_id(99)), None);
    }
}
