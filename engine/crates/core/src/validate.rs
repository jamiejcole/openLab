// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace validation (NFR-03). Problems that would make the show ambiguous are errors
//! (the file is refused); everything else is repaired or left for the cue to show as broken,
//! with a warning naming the cue.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;

use crate::cue::{Cue, CueKind, MAX_DB, SILENCE_DB};
use crate::model::{CueId, CueListId, NetworkPatchId, PatchId, Workspace, WorkspaceSettings};

/// Deepest allowed cue nesting (a top-level cue is depth 1). Keeps every valid workspace within
/// the JSON parser's recursion limit, so whatever saves also loads.
pub const MAX_GROUP_DEPTH: usize = 32;

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

/// What is wrong. Typed so the UI can render and localise it.
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
    /// The Audio cue's patch does not exist.
    PatchMissing { patch: PatchId },
    /// The Network cue's destination does not exist.
    NetworkPatchMissing { patch: NetworkPatchId },
    /// A time was negative, NaN or infinite; it was set to 0.
    InvalidTime { field: &'static str },
    /// A level was NaN or above [`MAX_DB`]; it was clamped (NaN becomes silence).
    InvalidLevel { field: &'static str },
    /// An Audio cue's end time was not after its start time; it was cleared (play to end).
    InvalidEndTime,
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
            IssueKind::PatchMissing { patch } => write!(f, "patch {} does not exist", patch.0),
            IssueKind::NetworkPatchMissing { patch } => {
                write!(f, "network patch {} does not exist", patch.0)
            }
            IssueKind::InvalidTime { field } => write!(f, "invalid {field}; set to 0"),
            IssueKind::InvalidLevel { field } => write!(f, "invalid {field}; clamped"),
            IssueKind::InvalidEndTime => write!(f, "end time not after start time; cleared"),
        }
    }
}

/// Reports every issue without changing the workspace. Same rules as [`repair`].
pub fn validate(workspace: &Workspace) -> Vec<ValidationIssue> {
    repair(&mut workspace.clone())
}

/// Reports every issue and repairs what can be repaired: clears duplicate cue numbers (later
/// cue in list order and depth-first group order loses), clamps invalid times and levels.
/// Error-severity issues are reported but not repaired.
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

#[derive(Default)]
struct Checker {
    issues: Vec<ValidationIssue>,
    cue_ids: HashSet<CueId>,
    numbers: HashMap<String, CueId>,
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

    fn time(&mut self, cue_id: Option<CueId>, field: &'static str, value: &mut f64) {
        if !(value.is_finite() && *value >= 0.0) {
            *value = 0.0;
            self.warn(cue_id, IssueKind::InvalidTime { field });
        }
    }

    fn level(&mut self, cue_id: Option<CueId>, field: &'static str, value: &mut f64) {
        if value.is_nan() {
            *value = SILENCE_DB;
            self.warn(cue_id, IssueKind::InvalidLevel { field });
        } else if *value > MAX_DB {
            *value = MAX_DB;
            self.warn(cue_id, IssueKind::InvalidLevel { field });
        } else if *value < SILENCE_DB {
            // Anything below silence is silence; normalise without a warning.
            *value = SILENCE_DB;
        }
    }

    fn settings(&mut self, settings: &mut WorkspaceSettings) {
        self.time(None, "min_go_interval", &mut settings.min_go_interval);
        self.time(None, "panic_duration", &mut settings.panic_duration);
        self.time(None, "autosave_interval", &mut settings.autosave_interval);
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
                start_time,
                end_time,
                fade_in,
                fade_out,
                slices,
                levels,
                ..
            } => {
                self.time(id, "start_time", start_time);
                if end_time.is_some_and(|end| !(end.is_finite() && end > *start_time)) {
                    *end_time = None;
                    self.warn(id, IssueKind::InvalidEndTime);
                }
                self.time(id, "fade_in.duration", &mut fade_in.duration);
                self.time(id, "fade_out.duration", &mut fade_out.duration);
                for slice in slices {
                    self.time(id, "slices.time", &mut slice.time);
                }
                self.level(id, "levels.master_db", &mut levels.master_db);
                for db in &mut levels.output_db {
                    self.level(id, "levels.output_db", db);
                }
                for crosspoint in &mut levels.crosspoints {
                    self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
                }
            }
            CueKind::Fade {
                duration, levels, ..
            } => {
                self.time(id, "duration", duration);
                if let Some(db) = &mut levels.master_db {
                    self.level(id, "levels.master_db", db);
                }
                for db in levels.output_db.iter_mut().flatten() {
                    self.level(id, "levels.output_db", db);
                }
                for crosspoint in &mut levels.crosspoints {
                    self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
                }
            }
            CueKind::Stop { fade_out, .. } => {
                self.time(id, "fade_out.duration", &mut fade_out.duration);
            }
            CueKind::Load { load_to_time, .. } => self.time(id, "load_to_time", load_to_time),
            CueKind::Wait { duration } => self.time(id, "duration", duration),
            CueKind::Group { .. }
            | CueKind::Start { .. }
            | CueKind::Pause { .. }
            | CueKind::Reset { .. }
            | CueKind::Devamp { .. }
            | CueKind::GoTo { .. }
            | CueKind::Arm { .. }
            | CueKind::Disarm { .. }
            | CueKind::Memo
            | CueKind::Network { .. } => {}
        }
    }

    fn references(&mut self, workspace: &Workspace) {
        let mut kinds: HashMap<CueId, &CueKind> = HashMap::new();
        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                kinds.entry(cue.id).or_insert(&cue.kind);
            });
        }
        let patches: HashSet<PatchId> = workspace.patches.iter().map(|p| p.id).collect();
        let network_patches: HashSet<NetworkPatchId> =
            workspace.network_patches.iter().map(|p| p.id).collect();

        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                let id = Some(cue.id);
                if let Some(target) = cue.kind.target() {
                    match kinds.get(&target) {
                        _ if target == cue.id => self.warn(id, IssueKind::TargetSelf),
                        None => self.warn(id, IssueKind::TargetMissing { target }),
                        Some(target_kind) if !can_target(&cue.kind, target_kind) => {
                            self.warn(id, IssueKind::TargetWrongKind { target });
                        }
                        Some(_) => {}
                    }
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
