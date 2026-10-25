// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace validation (NFR-03). Problems that would make the show ambiguous are errors
//! (the file is refused); everything else is repaired or left for the cue to show as broken,
//! with a warning naming the cue.
//!
//! After [`repair`], every number in the workspace is finite and in range, so the workspace
//! always saves and loads back unchanged.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;

use crate::cue::{Cue, FadeLevels};
use crate::cue::{
    CueKind, CueLevels, MAX_CHANNELS, MAX_DB, MAX_TIME, OscArg, SILENCE_DB, SliceMarker,
};
use crate::model::{
    CueId, CueListId, NetworkPatchId, OutputPatch, PatchId, PatchOutput, Workspace,
    WorkspaceSettings,
};

/// Deepest allowed cue nesting (a top-level cue is depth 1). Keeps every valid workspace within
/// the JSON parser's recursion limit, so whatever saves also loads.
pub const MAX_GROUP_DEPTH: usize = 32;
/// Longest patch output delay, milliseconds.
pub const MAX_DELAY_MS: f64 = 1000.0;
/// Largest auto-numbering start value and increment.
pub const MAX_AUTO_NUMBER: f64 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Repaired or left for the cue to show as broken; the workspace still loads.
    Warning,
    /// The workspace is refused.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidationIssue {
    pub severity: Severity,
    /// The cue the issue is about, if any.
    pub cue_id: Option<CueId>,
    pub kind: IssueKind,
}

/// What is wrong. Typed so the UI can render and localise it. `field` names the JSON field,
/// with nested objects joined by dots (e.g. `levels.master_db`, `settings.panic_duration`).
#[derive(Clone, Debug, PartialEq)]
pub enum IssueKind {
    /// Error: another cue already has `cue_id`.
    DuplicateCueId,
    /// Error.
    DuplicateCueListId { id: CueListId },
    /// Error.
    DuplicatePatchId { id: PatchId },
    /// Error.
    DuplicateNetworkPatchId { id: NetworkPatchId },
    /// Error: the cue is nested deeper than [`MAX_GROUP_DEPTH`].
    NestingTooDeep,
    /// `number` was already used by `first`; this cue's number was cleared.
    DuplicateCueNumber { number: String, first: CueId },
    /// The target cue does not exist.
    TargetMissing { target: CueId },
    /// The target cue's kind cannot be targeted by this cue (e.g. a Fade targeting a Memo).
    TargetWrongKind { target: CueId },
    /// The cue targets itself.
    TargetSelf,
    /// The Start or GoTo-and-GO cue would start itself again: through a chain of such cues, or
    /// by starting a group that contains it.
    TriggerCycle,
    /// The Audio cue's patch does not exist.
    PatchMissing { patch: PatchId },
    /// The Network cue's destination does not exist.
    NetworkPatchMissing { patch: NetworkPatchId },
    /// A time was NaN, negative or above its maximum (24 h unless documented); it was clamped.
    InvalidTime { field: &'static str },
    /// A level was NaN or above [`MAX_DB`]; it was clamped (NaN becomes silence).
    InvalidLevel { field: &'static str },
    /// Some other number was out of range or not finite; it was clamped or reset.
    InvalidValue { field: &'static str },
    /// An Audio cue's end time was not after its start time; it was cleared (play to end).
    InvalidEndTime,
    /// A slice marker at `time` was at or outside the trim points; it was removed.
    SliceOutsideTrim { time: f64 },
    /// A second slice marker at `time` was removed.
    DuplicateSlice { time: f64 },
    /// Slice markers were out of order; they were sorted.
    SlicesUnsorted,
    /// `count` entries of `field` referred to channels or outputs that do not exist; removed.
    IndexOutOfRange { field: &'static str, count: usize },
    /// A patch had no outputs (one was added) or more than [`MAX_CHANNELS`] (extras removed).
    PatchChannelCount { patch: PatchId },
    /// A patch level or delay was out of range; it was clamped.
    InvalidPatchValue { patch: PatchId, field: &'static str },
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(id) = self.cue_id {
            write!(f, "cue {}: ", id.0)?;
        }
        match &self.kind {
            IssueKind::DuplicateCueId => write!(f, "duplicate cue id"),
            IssueKind::DuplicateCueListId { id } => write!(f, "duplicate cue list id {}", id.0),
            IssueKind::DuplicatePatchId { id } => write!(f, "duplicate patch id {}", id.0),
            IssueKind::DuplicateNetworkPatchId { id } => {
                write!(f, "duplicate network patch id {}", id.0)
            }
            IssueKind::NestingTooDeep => {
                write!(f, "nested deeper than {MAX_GROUP_DEPTH} levels")
            }
            IssueKind::DuplicateCueNumber { number, first } => write!(
                f,
                "number \"{number}\" is already used by cue {}; cleared",
                first.0
            ),
            IssueKind::TargetMissing { target } => write!(f, "target {} does not exist", target.0),
            IssueKind::TargetWrongKind { target } => {
                write!(f, "target {} is the wrong kind of cue", target.0)
            }
            IssueKind::TargetSelf => write!(f, "targets itself"),
            IssueKind::TriggerCycle => write!(f, "would start itself again (trigger cycle)"),
            IssueKind::PatchMissing { patch } => write!(f, "patch {} does not exist", patch.0),
            IssueKind::NetworkPatchMissing { patch } => {
                write!(f, "network patch {} does not exist", patch.0)
            }
            IssueKind::InvalidTime { field } => write!(f, "{field} out of range; clamped"),
            IssueKind::InvalidLevel { field } => write!(f, "invalid {field}; clamped"),
            IssueKind::InvalidValue { field } => write!(f, "invalid {field}; corrected"),
            IssueKind::InvalidEndTime => write!(f, "end time not after start time; cleared"),
            IssueKind::SliceOutsideTrim { time } => {
                write!(f, "slice at {time} s is outside the trim points; removed")
            }
            IssueKind::DuplicateSlice { time } => write!(f, "duplicate slice at {time} s; removed"),
            IssueKind::SlicesUnsorted => write!(f, "slices out of order; sorted"),
            IssueKind::IndexOutOfRange { field, count } => {
                write!(
                    f,
                    "{count} {field} entries refer to missing channels; removed"
                )
            }
            IssueKind::PatchChannelCount { patch } => write!(
                f,
                "patch {} must have 1 to {MAX_CHANNELS} outputs; corrected",
                patch.0
            ),
            IssueKind::InvalidPatchValue { patch, field } => {
                write!(f, "patch {}: invalid {field}; clamped", patch.0)
            }
        }
    }
}

/// Reports every issue without changing the workspace. Same rules as [`repair`].
pub fn validate(workspace: &Workspace) -> Vec<ValidationIssue> {
    repair(&mut workspace.clone())
}

/// Reports every issue and repairs what can be repaired: clears duplicate cue numbers (later
/// cue in list order and depth-first group order loses), clamps out-of-range numbers, tidies
/// slices and drops references to channels that do not exist. Error-severity issues are
/// reported but not repaired.
pub fn repair(workspace: &mut Workspace) -> Vec<ValidationIssue> {
    let mut checker = Checker::default();
    for id in duplicates(workspace.cue_lists.iter().map(|l| l.id)) {
        checker.error(None, IssueKind::DuplicateCueListId { id });
    }
    for id in duplicates(workspace.patches.iter().map(|p| p.id)) {
        checker.error(None, IssueKind::DuplicatePatchId { id });
    }
    for id in duplicates(workspace.network_patches.iter().map(|p| p.id)) {
        checker.error(None, IssueKind::DuplicateNetworkPatchId { id });
    }
    checker.settings(&mut workspace.settings);
    for patch in &mut workspace.patches {
        checker.patch(patch);
    }
    for list in &mut workspace.cue_lists {
        checker.cues(&mut list.cues, 1);
    }
    checker.references(workspace);
    checker.issues
}

fn duplicates<T: Copy + Eq + Hash>(ids: impl Iterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    ids.filter(|id| !seen.insert(*id)).collect()
}

/// Clamps `value` into `lo..=hi` (NaN becomes `lo`). Returns whether it changed.
fn clamp(value: &mut f64, lo: f64, hi: f64) -> bool {
    let fixed = if value.is_nan() {
        lo
    } else {
        value.clamp(lo, hi)
    };
    let changed = fixed != *value;
    *value = fixed;
    changed
}

/// Removes items failing `keep`; returns how many were removed.
fn retain_counted<T>(items: &mut Vec<T>, keep: impl FnMut(&T) -> bool) -> usize {
    let before = items.len();
    items.retain(keep);
    before - items.len()
}

#[derive(Default)]
struct Checker {
    issues: Vec<ValidationIssue>,
    cue_ids: HashSet<CueId>,
    numbers: HashMap<String, CueId>,
    /// Output count of each patch, after repair.
    patch_outputs: HashMap<PatchId, usize>,
}

impl Checker {
    fn warn(&mut self, cue_id: Option<CueId>, kind: IssueKind) {
        self.issues.push(ValidationIssue {
            severity: Severity::Warning,
            cue_id,
            kind,
        });
    }

    fn error(&mut self, cue_id: Option<CueId>, kind: IssueKind) {
        self.issues.push(ValidationIssue {
            severity: Severity::Error,
            cue_id,
            kind,
        });
    }

    fn time_in(
        &mut self,
        cue_id: Option<CueId>,
        field: &'static str,
        value: &mut f64,
        lo: f64,
        hi: f64,
    ) {
        if clamp(value, lo, hi) {
            self.warn(cue_id, IssueKind::InvalidTime { field });
        }
    }

    fn time(&mut self, cue_id: Option<CueId>, field: &'static str, value: &mut f64) {
        self.time_in(cue_id, field, value, 0.0, MAX_TIME);
    }

    fn level(&mut self, cue_id: Option<CueId>, field: &'static str, value: &mut f64) {
        if value.is_nan() || *value > MAX_DB {
            clamp(value, SILENCE_DB, MAX_DB);
            self.warn(cue_id, IssueKind::InvalidLevel { field });
        } else if *value < SILENCE_DB {
            // Anything below silence is silence; normalise without a warning.
            *value = SILENCE_DB;
        }
    }

    fn count(&mut self, cue_id: Option<CueId>, field: &'static str, count: usize) {
        if count > 0 {
            self.warn(cue_id, IssueKind::IndexOutOfRange { field, count });
        }
    }

    fn settings(&mut self, s: &mut WorkspaceSettings) {
        self.time_in(
            None,
            "settings.min_go_interval",
            &mut s.min_go_interval,
            0.0,
            10.0,
        );
        self.time_in(
            None,
            "settings.panic_duration",
            &mut s.panic_duration,
            0.0,
            60.0,
        );
        self.time_in(
            None,
            "settings.autosave_interval",
            &mut s.autosave_interval,
            5.0,
            3600.0,
        );
        self.level(None, "settings.master_db", &mut s.master_db);
        if clamp(&mut s.auto_number_start, 0.0, MAX_AUTO_NUMBER) {
            self.warn(
                None,
                IssueKind::InvalidValue {
                    field: "settings.auto_number_start",
                },
            );
        }
        if !(s.auto_number_increment > 0.0 && s.auto_number_increment <= MAX_AUTO_NUMBER) {
            s.auto_number_increment = if s.auto_number_increment > 0.0 {
                MAX_AUTO_NUMBER
            } else {
                1.0
            };
            self.warn(
                None,
                IssueKind::InvalidValue {
                    field: "settings.auto_number_increment",
                },
            );
        }
    }

    fn patch(&mut self, patch: &mut OutputPatch) {
        let id = patch.id;
        let max = MAX_CHANNELS as usize;
        if patch.outputs.is_empty() || patch.outputs.len() > max {
            patch.outputs.truncate(max);
            if patch.outputs.is_empty() {
                patch.outputs.push(PatchOutput {
                    level_db: 0.0,
                    mute: false,
                    delay_ms: 0.0,
                    polarity_inverted: false,
                });
            }
            self.warn(None, IssueKind::PatchChannelCount { patch: id });
        }
        let mut bad = Vec::new();
        let master = patch.master_db;
        if master.is_nan() || master > MAX_DB {
            bad.push("master_db");
        }
        clamp(&mut patch.master_db, SILENCE_DB, MAX_DB);
        for output in &mut patch.outputs {
            if output.level_db.is_nan() || output.level_db > MAX_DB {
                bad.push("outputs.level_db");
            }
            clamp(&mut output.level_db, SILENCE_DB, MAX_DB);
            if clamp(&mut output.delay_ms, 0.0, MAX_DELAY_MS) {
                bad.push("outputs.delay_ms");
            }
        }
        for field in bad {
            self.warn(None, IssueKind::InvalidPatchValue { patch: id, field });
        }
        self.patch_outputs.entry(id).or_insert(patch.outputs.len());
    }

    fn cues(&mut self, cues: &mut [Cue], depth: usize) {
        for cue in cues {
            let id = Some(cue.id);
            if !self.cue_ids.insert(cue.id) {
                self.error(id, IssueKind::DuplicateCueId);
            }
            if depth == MAX_GROUP_DEPTH + 1 {
                self.error(id, IssueKind::NestingTooDeep);
            }
            if let Some(number) = cue.number.take() {
                if let Some(&first) = self.numbers.get(&number) {
                    self.warn(id, IssueKind::DuplicateCueNumber { number, first });
                } else if !number.is_empty() {
                    self.numbers.insert(number.clone(), cue.id);
                    cue.number = Some(number);
                }
            }
            self.time(id, "pre_wait", &mut cue.pre_wait);
            self.time(id, "post_wait", &mut cue.post_wait);
            self.kind_values(id, &mut cue.kind);
            if let CueKind::Group { children, .. } = &mut cue.kind {
                self.cues(children, depth + 1);
            }
        }
    }

    fn kind_values(&mut self, id: Option<CueId>, kind: &mut CueKind) {
        match kind {
            CueKind::Audio {
                patch,
                start_time,
                end_time,
                fade_in,
                fade_out,
                slices,
                levels,
                ..
            } => {
                self.time(id, "start_time", start_time);
                if let Some(mut end) = *end_time {
                    if end.is_finite() && end > MAX_TIME {
                        end = MAX_TIME;
                        self.warn(id, IssueKind::InvalidTime { field: "end_time" });
                    }
                    *end_time = (end.is_finite() && end > *start_time).then_some(end);
                    if end_time.is_none() {
                        self.warn(id, IssueKind::InvalidEndTime);
                    }
                }
                self.time(id, "fade_in.duration", &mut fade_in.duration);
                self.time(id, "fade_out.duration", &mut fade_out.duration);
                self.slices(id, slices, *start_time, *end_time);
                let outputs = self
                    .patch_outputs
                    .get(patch)
                    .copied()
                    .unwrap_or(MAX_CHANNELS as usize);
                self.cue_levels(id, levels, outputs);
            }
            CueKind::Fade {
                duration, levels, ..
            } => {
                self.time(id, "duration", duration);
                self.fade_levels(id, levels);
            }
            CueKind::Stop { fade_out, .. } => {
                self.time(id, "fade_out.duration", &mut fade_out.duration);
            }
            CueKind::Load { load_to_time, .. } => self.time(id, "load_to_time", load_to_time),
            CueKind::Wait { duration } => self.time(id, "duration", duration),
            CueKind::Network { args, .. } => {
                for arg in args {
                    // An f32 read from a larger JSON number becomes infinite.
                    if let OscArg::Float { value } = arg
                        && !value.is_finite()
                    {
                        *value = 0.0;
                        self.warn(
                            id,
                            IssueKind::InvalidValue {
                                field: "args.value",
                            },
                        );
                    }
                }
            }
            CueKind::Group { .. }
            | CueKind::Start { .. }
            | CueKind::Pause { .. }
            | CueKind::Reset { .. }
            | CueKind::Devamp { .. }
            | CueKind::GoTo { .. }
            | CueKind::Arm { .. }
            | CueKind::Disarm { .. }
            | CueKind::Memo => {}
        }
    }

    fn slices(
        &mut self,
        id: Option<CueId>,
        slices: &mut Vec<SliceMarker>,
        start: f64,
        end: Option<f64>,
    ) {
        for slice in slices.iter_mut() {
            self.time(id, "slices.time", &mut slice.time);
        }
        let mut outside = Vec::new();
        slices.retain(|s| {
            let inside = s.time > start && end.is_none_or(|end| s.time < end);
            if !inside {
                outside.push(s.time);
            }
            inside
        });
        for time in outside {
            self.warn(id, IssueKind::SliceOutsideTrim { time });
        }
        if !slices.is_sorted_by(|a, b| a.time <= b.time) {
            // Stable, so the first of equal markers keeps its play count.
            slices.sort_by(|a, b| a.time.total_cmp(&b.time));
            self.warn(id, IssueKind::SlicesUnsorted);
        }
        let mut duplicates = Vec::new();
        slices.dedup_by(|later, earlier| {
            let duplicate = later.time == earlier.time;
            if duplicate {
                duplicates.push(later.time);
            }
            duplicate
        });
        for time in duplicates {
            self.warn(id, IssueKind::DuplicateSlice { time });
        }
    }

    fn cue_levels(&mut self, id: Option<CueId>, levels: &mut CueLevels, patch_outputs: usize) {
        self.level(id, "levels.master_db", &mut levels.master_db);
        let extra = levels.outputs.len().saturating_sub(patch_outputs);
        levels.outputs.truncate(patch_outputs);
        self.count(id, "levels.outputs", extra);
        for output in &mut levels.outputs {
            self.level(id, "levels.outputs.db", &mut output.db);
        }
        let outputs = levels.outputs.len();
        let dropped = retain_counted(&mut levels.crosspoints, |c| {
            c.input < MAX_CHANNELS && (c.output as usize) < outputs
        });
        self.count(id, "levels.crosspoints", dropped);
        for crosspoint in &mut levels.crosspoints {
            self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
        }
        let mut dropped = 0;
        for gang in &mut levels.gangs {
            dropped += retain_counted(gang, |&i| (i as usize) < outputs);
        }
        levels.gangs.retain(|gang| !gang.is_empty());
        self.count(id, "levels.gangs", dropped);
    }

    fn fade_levels(&mut self, id: Option<CueId>, levels: &mut FadeLevels) {
        if let Some(db) = &mut levels.master_db {
            self.level(id, "levels.master_db", db);
        }
        let max = MAX_CHANNELS as usize;
        let extra = levels.output_db.len().saturating_sub(max);
        levels.output_db.truncate(max);
        self.count(id, "levels.output_db", extra);
        for db in levels.output_db.iter_mut().flatten() {
            self.level(id, "levels.output_db", db);
        }
        let dropped = retain_counted(&mut levels.crosspoints, |c| {
            c.input < MAX_CHANNELS && c.output < MAX_CHANNELS
        });
        self.count(id, "levels.crosspoints", dropped);
        for crosspoint in &mut levels.crosspoints {
            self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
        }
    }

    fn references(&mut self, workspace: &Workspace) {
        let mut by_id: HashMap<CueId, &Cue> = HashMap::new();
        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                by_id.entry(cue.id).or_insert(cue);
            });
        }
        let patches: HashSet<PatchId> = workspace.patches.iter().map(|p| p.id).collect();
        let network_patches: HashSet<NetworkPatchId> =
            workspace.network_patches.iter().map(|p| p.id).collect();

        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                let id = Some(cue.id);
                if let Some(target) = cue.kind.target() {
                    match by_id.get(&target) {
                        _ if target == cue.id => self.warn(id, IssueKind::TargetSelf),
                        None => self.warn(id, IssueKind::TargetMissing { target }),
                        Some(t) if !can_target(&cue.kind, &t.kind) => {
                            self.warn(id, IssueKind::TargetWrongKind { target });
                        }
                        Some(_) => {}
                    }
                }
                if let Some(target) = starts(&cue.kind)
                    && target != cue.id
                    && reaches(&by_id, target, cue.id)
                {
                    self.warn(id, IssueKind::TriggerCycle);
                }
                match &cue.kind {
                    CueKind::Audio { patch, .. } if !patches.contains(patch) => {
                        self.warn(id, IssueKind::PatchMissing { patch: *patch });
                    }
                    CueKind::Network { destination, .. }
                        if !network_patches.contains(destination) =>
                    {
                        self.warn(
                            id,
                            IssueKind::NetworkPatchMissing {
                                patch: *destination,
                            },
                        );
                    }
                    _ => {}
                }
            });
        }
    }
}

/// The cue this cue starts when it runs, if it starts one.
fn starts(kind: &CueKind) -> Option<CueId> {
    match kind {
        CueKind::Start { target }
        | CueKind::GoTo {
            target,
            then_go: true,
        } => *target,
        _ => None,
    }
}

/// Whether starting `from` can lead to `to` starting: through Start/GoTo-and-GO targets, and
/// through groups to all their children (whatever the mode, to stay on the safe side).
// ponytail: one search per triggering cue, O(n·(n+e)); fine for thousands of cues, use SCCs if not.
fn reaches(by_id: &HashMap<CueId, &Cue>, from: CueId, to: CueId) -> bool {
    let mut stack = vec![from];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if id == to {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(cue) = by_id.get(&id) {
            stack.extend(cue.children().iter().map(|child| child.id));
            stack.extend(starts(&cue.kind));
        }
    }
    false
}

/// Fades act on audio levels (a Group fades its children); Devamp needs slices. Other control
/// cues accept any target.
fn can_target(source: &CueKind, target: &CueKind) -> bool {
    match source {
        CueKind::Fade { .. } => matches!(target, CueKind::Audio { .. } | CueKind::Group { .. }),
        CueKind::Devamp { .. } => matches!(target, CueKind::Audio { .. }),
        _ => true,
    }
}

fn visit<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    for cue in cues {
        f(cue);
        visit(cue.children(), f);
    }
}
