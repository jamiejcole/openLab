// SPDX-License-Identifier: GPL-3.0-or-later

//! Workspace validation (NFR-03). Problems that would make the show ambiguous are errors
//! (the file is refused); everything else is repaired or left for the cue to show as broken,
//! with a warning naming the cue.
//!
//! After [`repair`], every number in the workspace is finite and in range, so the workspace
//! always saves and loads back unchanged.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;

use crate::cue::{Cue, FadeLevels, GroupMode, Hotkey};
use crate::cue::{
    CueKind, CueLevels, MAX_CHANNELS, MAX_DB, MAX_TIME, OscArg, SILENCE_DB, SliceMarker,
};
use crate::model::{
    CueId, CueListId, NetworkPatchId, OutputPatch, PatchId, PatchOutput, Workspace,
    WorkspaceSettings, hotkey_field,
};
use crate::network::MAX_HOST_BYTES;
use crate::osc_access::{
    MAX_OSC_PASSCODE_BYTES, MAX_OSC_PASSCODES, OscAccess, is_valid_new_passcode,
};

/// Longest name of a workspace, cue list, patch or network patch, in Unicode scalar values (not
/// bytes: a name in any script gets the same room, and a UI text field counts characters).
/// QLab 5 documents no limit (docs/todo/open-qlab-questions.md, row 13).
pub const MAX_NAME_SCALARS: usize = 256;

/// Longest cue name, in bytes: 256 characters of any script (at most 4 bytes each), as for the
/// names [`MAX_NAME_SCALARS`] bounds. Cue names are free text otherwise (QLab 5 sets no rule).
pub const MAX_CUE_NAME_BYTES: usize = 4 * MAX_NAME_SCALARS;

/// Longest cue notes, in bytes (the bound on a Network cue's string argument too).
pub const MAX_CUE_NOTES_BYTES: usize = 64 * 1024;

/// Hard bounds on a Network cue's address and arguments. QLab 5 accepts a malformed message and
/// shows the cue broken until it is fixed (manual pp. 371-372), so the send limits in `network`
/// are not enforced at the edit: the cue engine reports `network_message_invalid` and never
/// sends. These bounds, far above the send limits (512 / 32 / 4096 bytes), only keep a hostile
/// or broken client from filling the document with megabytes through one command. Load keeps a
/// value over them and warns ([`IssueKind::TooLarge`]).
pub const MAX_NETWORK_ADDRESS_BYTES: usize = 64 * 1024;
pub const MAX_NETWORK_ARGS: usize = 1024;
pub const MAX_NETWORK_STRING_BYTES: usize = 64 * 1024;

/// Why `name` is refused as a new name (workspace, cue list, patch, network patch): too long, or
/// holding a control character (NUL, a newline, a tab, escape codes), which no single-line name
/// field can show. Load keeps such a name and warns ([`IssueKind::InvalidName`]).
pub fn name_problem(name: &str) -> Option<&'static str> {
    // Bounded scan: stops at the first control character or just past the limit.
    if name.chars().take(MAX_NAME_SCALARS + 1).count() > MAX_NAME_SCALARS {
        Some("a name is at most 256 characters")
    } else if name.chars().any(char::is_control) {
        Some("a name cannot contain control characters")
    } else {
        None
    }
}

/// Why `host` is refused as a new network patch host: longer than a DNS name
/// ([`MAX_HOST_BYTES`]) or holding a control character. Whether it can be addressed at all is
/// [`check_endpoint`](crate::check_endpoint)'s question and makes a Network cue broken, not the
/// edit refused.
pub fn host_problem(host: &str) -> Option<&'static str> {
    if host.len() > MAX_HOST_BYTES {
        Some("a host is at most 253 bytes")
    } else if host.chars().any(char::is_control) {
        Some("a host cannot contain control characters")
    } else {
        None
    }
}

/// Deepest allowed cue nesting (a top-level cue is depth 1). Keeps every valid workspace within
/// the JSON parser's recursion limit, so whatever saves also loads.
pub const MAX_GROUP_DEPTH: usize = 32;
/// Longest patch output delay, milliseconds.
pub const MAX_DELAY_MS: f64 = 1000.0;
/// Largest auto-numbering start value and increment.
pub const MAX_AUTO_NUMBER: f64 = 1_000_000.0;
/// Shortest slice of an Audio cue, seconds: between its trim points and slice markers, every
/// slice (and so every loop) is at least this long. QLab 5 refuses a shorter slice or trim in its
/// editor, and plays a loop of exactly this length (M2.24, QLab check D6).
pub const MIN_SLICE_SECONDS: f64 = 0.051;

/// Whether a slice of `seconds` is shorter than [`MIN_SLICE_SECONDS`]. With a margin far below
/// one sample, so that times typed in milliseconds (`1.051 - 1.0`) are not refused for the
/// rounding of their difference.
fn slice_too_short(seconds: f64) -> bool {
    seconds < MIN_SLICE_SECONDS - 1e-9
}

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
    /// An Audio cue's end time was less than [`MIN_SLICE_SECONDS`] after its start time; it was
    /// moved to exactly that (`end`), the shortest trim QLab 5 allows.
    EndTimeTooShort { end: f64 },
    /// A slice marker at `time` was at or outside the trim points; it was removed.
    SliceOutsideTrim { time: f64 },
    /// A second slice marker at `time` was removed.
    DuplicateSlice { time: f64 },
    /// The slice marker at `time` made a slice shorter than [`MIN_SLICE_SECONDS`] (counted from
    /// the start time or the marker before it, or up to the end time) and the slice next to it
    /// left no room to lengthen it: it was removed, and the merged slice plays as the longer of
    /// the two did.
    SliceTooShort { time: f64 },
    /// The slice marker at `from` made a slice shorter than [`MIN_SLICE_SECONDS`]; it was moved
    /// to `to`, which makes that slice exactly that long, keeping its play count.
    SliceMoved { from: f64, to: f64 },
    /// Slice markers were out of order; they were sorted.
    SlicesUnsorted,
    /// `count` entries of `field` referred to channels or outputs that do not exist; removed.
    IndexOutOfRange { field: &'static str, count: usize },
    /// The Audio cue has `count` more outputs than its patch; they are kept but silent.
    UnpatchedOutputs { count: usize },
    /// `count` repeated entries of `field` were removed (the first is kept).
    DuplicateEntries { field: &'static str, count: usize },
    /// A hotkey's key name is empty or has upper-case letters, whitespace or control
    /// characters, so it would never fire. Left as is.
    InvalidHotkey { field: &'static str },
    /// A patch had no outputs (one was added) or more than [`MAX_CHANNELS`] (extras removed).
    PatchChannelCount { patch: PatchId },
    /// A patch level or delay was out of range; it was clamped.
    InvalidPatchValue { patch: PatchId, field: &'static str },
    /// `second_trigger` is `play_next` or `play_previous` on a cue that is not a Playlist group:
    /// kept, but a second trigger does nothing (as `nothing`) until the cue is one.
    SecondTriggerNeedsPlaylist,
    /// The cue's hotkey is the workspace's key for `action` (a field of `settings.hotkeys`, e.g.
    /// `go` for Space): kept, but the workspace action wins and the key never starts the cue
    /// (QLab 5's "Hotkey used by workspace", check K1b).
    HotkeyUsedByWorkspace { action: &'static str },
    /// Warning, only when loading a show file older than version 3: this Fade cue targets a
    /// group, and since version 3 a group Fade moves each child relative to its level (QLab 5,
    /// check F2) where it used to set it, so the cue may now sound different. Kept as it is.
    GroupFadeNowRelative,
    /// An OSC rights set had Edit or Control without View: they were cleared. `row` is the
    /// passcode row (`None`: the No Passcode row).
    OscRightsNeedView { row: Option<usize> },
    /// The OSC passcode in row `row` was removed (`reason`: empty, too long, a repeat, or past
    /// the table's size limit). Passcodes are never named: they are secret.
    OscPasscodeDropped { row: usize, reason: &'static str },
    /// The OSC passcode in row `row` is not made of digits only (QLab's are numeric): kept, since
    /// removing it would change who can connect, but the user should replace it.
    OscPasscodeNotDigits { row: usize },
    /// The name or host in `field` (`name`, `cue_lists.name`, `patches.name`,
    /// `network_patches.name`, `network_patches.host`) is over-long or has a control character:
    /// kept as it is, since load never refuses a show; editing it needs a valid value.
    InvalidName { field: &'static str },
    /// The cue's `field` (`name`, `notes`, `address`, `args`, `args.value`) is over its memory
    /// bound of `limit` (bytes; entries for `args`): kept as it is, since load never refuses a
    /// show; an edit cannot put such a value in a cue.
    TooLarge { field: &'static str, limit: usize },
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
            IssueKind::EndTimeTooShort { end } => write!(
                f,
                "end time less than {} ms after start time; moved to {end} s",
                MIN_SLICE_SECONDS * 1000.0
            ),
            IssueKind::SliceOutsideTrim { time } => {
                write!(f, "slice at {time} s is outside the trim points; removed")
            }
            IssueKind::DuplicateSlice { time } => write!(f, "duplicate slice at {time} s; removed"),
            IssueKind::SliceMoved { from, to } => write!(
                f,
                "slice at {from} s makes a slice shorter than {} ms; moved to {to} s",
                MIN_SLICE_SECONDS * 1000.0
            ),
            IssueKind::SliceTooShort { time } => write!(
                f,
                "slice at {time} s makes a slice shorter than {} ms; removed",
                MIN_SLICE_SECONDS * 1000.0
            ),
            IssueKind::SlicesUnsorted => write!(f, "slices out of order; sorted"),
            IssueKind::IndexOutOfRange { field, count } => {
                write!(
                    f,
                    "{count} {field} entries refer to missing channels; removed"
                )
            }
            IssueKind::UnpatchedOutputs { count } => {
                write!(f, "{count} outputs beyond the patch's outputs are silent")
            }
            IssueKind::DuplicateEntries { field, count } => {
                write!(f, "{count} repeated {field} entries removed")
            }
            IssueKind::InvalidHotkey { field } => write!(f, "invalid key name in {field}"),
            IssueKind::PatchChannelCount { patch } => write!(
                f,
                "patch {} must have 1 to {MAX_CHANNELS} outputs; corrected",
                patch.0
            ),
            IssueKind::HotkeyUsedByWorkspace { action } => write!(
                f,
                "hotkey is the workspace's {action} key, which wins: it does not start this cue"
            ),
            IssueKind::GroupFadeNowRelative => write!(
                f,
                "this Fade targets a group: fades aimed at groups now move each child relative \
                 to its own level, as in QLab 5, instead of setting it; check that it still does \
                 what the show needs"
            ),
            IssueKind::OscRightsNeedView { row: Some(row) } => write!(
                f,
                "OSC passcode row {row}: Edit and Control need View; they were cleared"
            ),
            IssueKind::OscRightsNeedView { row: None } => write!(
                f,
                "OSC No Passcode rights: Edit and Control need View; they were cleared"
            ),
            IssueKind::OscPasscodeDropped { row, reason } => {
                write!(f, "OSC passcode row {row} removed: {reason}")
            }
            IssueKind::OscPasscodeNotDigits { row } => write!(
                f,
                "OSC passcode row {row} is not made of digits only; replace it"
            ),
            IssueKind::InvalidName { field } => write!(
                f,
                "{field} is too long or has a control character; kept as it is"
            ),
            IssueKind::TooLarge { field, limit } => {
                let unit = if *field == "args" { "entries" } else { "bytes" };
                write!(f, "{field} is longer than {limit} {unit}")
            }
            IssueKind::SecondTriggerNeedsPlaylist => write!(
                f,
                "second trigger plays the next or previous child, which only a playlist group \
                 has; it does nothing here"
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
    checker.names(workspace);
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

/// Issues `cues` would raise on their own if placed at `depth` (a top-level cue is depth 1):
/// ranges, slices, hotkeys, nesting, and duplicate ids or numbers among themselves. Does not
/// look at the rest of the workspace, nor at references. Commands use it so an edit never
/// introduces something [`repair`] would have to fix. The caller bounds the nesting first
/// (this recurses).
pub(crate) fn subtree_issues(
    workspace: &Workspace,
    cues: &[Cue],
    depth: usize,
) -> Vec<ValidationIssue> {
    let mut checker = Checker::default();
    for patch in &workspace.patches {
        checker.patch_outputs.insert(patch.id, patch.outputs.len());
    }
    let mut repaired = cues.to_vec();
    checker.cues(&mut repaired, depth);
    // Some repairs are silent (levels below silence, empty gangs, empty numbers). Whatever
    // repair would change counts, so a command's result always saves and loads back unchanged.
    let reported =
        (checker.issues.iter()).any(|i| !matches!(i.kind, IssueKind::UnpatchedOutputs { .. }));
    if !reported && repaired != cues {
        checker.warn(
            cues.first().map(|cue| cue.id),
            IssueKind::InvalidValue { field: "cue" },
        );
    }
    checker.issues
}

/// Issues `settings` would raise on load, including silent repairs (a command's result must
/// save and load back unchanged).
pub(crate) fn settings_problems(settings: &WorkspaceSettings) -> Vec<ValidationIssue> {
    let mut repaired = settings.clone();
    let mut checker = Checker::default();
    checker.settings(&mut repaired);
    if repaired != *settings {
        checker.warn(None, IssueKind::InvalidValue { field: "settings" });
    }
    checker.issues
}

/// As [`settings_problems`], for one output patch.
pub(crate) fn patch_problems(patch: &OutputPatch) -> Vec<ValidationIssue> {
    let mut repaired = patch.clone();
    let mut checker = Checker::default();
    checker.patch(&mut repaired);
    if checker.issues.is_empty() && repaired != *patch {
        checker.warn(
            None,
            IssueKind::InvalidPatchValue {
                patch: patch.id,
                field: "patch",
            },
        );
    }
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

/// Removes later items whose key was already seen; returns how many were removed.
fn dedupe<T, K: Eq + Hash>(items: &mut Vec<T>, key: impl Fn(&T) -> K) -> usize {
    let mut seen = HashSet::new();
    retain_counted(items, |item| seen.insert(key(item)))
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

    fn too_large(&mut self, cue_id: Option<CueId>, field: &'static str, len: usize, limit: usize) {
        if len > limit {
            self.warn(cue_id, IssueKind::TooLarge { field, limit });
        }
    }

    fn count(&mut self, cue_id: Option<CueId>, field: &'static str, count: usize) {
        if count > 0 {
            self.warn(cue_id, IssueKind::IndexOutOfRange { field, count });
        }
    }

    fn duplicates(&mut self, cue_id: Option<CueId>, field: &'static str, count: usize) {
        if count > 0 {
            self.warn(cue_id, IssueKind::DuplicateEntries { field, count });
        }
    }

    fn hotkey(&mut self, cue_id: Option<CueId>, field: &'static str, hotkey: Option<&Hotkey>) {
        if let Some(hotkey) = hotkey {
            let key = &hotkey.key;
            if key.is_empty()
                || key
                    .chars()
                    .any(|c| c.is_uppercase() || c.is_whitespace() || c.is_control())
            {
                self.warn(cue_id, IssueKind::InvalidHotkey { field });
            }
        }
    }

    /// The OSC passcode table (M4.5c): rights need View; rows that are empty, too long, repeated
    /// or past the table's size are removed (the first of a repeat stays). A passcode that is
    /// not digits is kept and pointed out: dropping it would silently lock a console out of a
    /// show migrated from a file whose passcode was any text.
    fn osc_access(&mut self, access: &mut OscAccess) {
        if !access.no_passcode.is_consistent() {
            access.no_passcode = access.no_passcode.consistent();
            self.warn(None, IssueKind::OscRightsNeedView { row: None });
        }
        let mut seen: HashSet<String> = HashSet::new();
        let rows = std::mem::take(&mut access.passcodes);
        for (row, mut entry) in rows.into_iter().enumerate() {
            let reason = if entry.passcode.is_empty() {
                Some("it is empty")
            } else if entry.passcode.len() > MAX_OSC_PASSCODE_BYTES {
                Some("it is too long")
            } else if seen.contains(&entry.passcode) {
                Some("it repeats an earlier row")
            } else if access.passcodes.len() >= MAX_OSC_PASSCODES {
                Some("the table is full")
            } else {
                None
            };
            if let Some(reason) = reason {
                self.warn(None, IssueKind::OscPasscodeDropped { row, reason });
                continue;
            }
            if !entry.rights.is_consistent() {
                entry.rights = entry.rights.consistent();
                self.warn(None, IssueKind::OscRightsNeedView { row: Some(row) });
            }
            if !is_valid_new_passcode(&entry.passcode) {
                self.warn(None, IssueKind::OscPasscodeNotDigits { row });
            }
            seen.insert(entry.passcode.clone());
            access.passcodes.push(entry);
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
        self.osc_access(&mut s.osc_access);
        for (name, key) in s.hotkeys.slots() {
            self.hotkey(None, hotkey_field(name), key.as_ref());
        }
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
            self.too_large(id, "name", cue.name.len(), MAX_CUE_NAME_BYTES);
            self.too_large(id, "notes", cue.notes.len(), MAX_CUE_NOTES_BYTES);
            self.hotkey(id, "triggers.hotkey", cue.triggers.hotkey.as_ref());
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
                last_slice_play_count,
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
                    } else if slice_too_short(end - *start_time) {
                        // Kept as close as allowed: a trim shorter than QLab's floor becomes the
                        // floor, rather than the whole file (beyond the longest time: cleared).
                        let floor = *start_time + MIN_SLICE_SECONDS;
                        *end_time = (floor <= MAX_TIME).then_some(floor);
                        match *end_time {
                            Some(end) => self.warn(id, IssueKind::EndTimeTooShort { end }),
                            None => self.warn(id, IssueKind::InvalidEndTime),
                        }
                    }
                }
                self.time(id, "fade_in.duration", &mut fade_in.duration);
                self.time(id, "fade_out.duration", &mut fade_out.duration);
                self.slices(id, slices, last_slice_play_count, *start_time, *end_time);
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
            CueKind::Network { address, args, .. } => {
                self.too_large(id, "address", address.len(), MAX_NETWORK_ADDRESS_BYTES);
                self.too_large(id, "args", args.len(), MAX_NETWORK_ARGS);
                let longest = args
                    .iter()
                    .map(|a| match a {
                        OscArg::String { value } => value.len(),
                        _ => 0,
                    })
                    .max()
                    .unwrap_or(0);
                self.too_large(id, "args.value", longest, MAX_NETWORK_STRING_BYTES);
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
        last_play_count: &mut u32,
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
        // Every slice at least `MIN_SLICE_SECONDS`, changing the cue as little as possible. A
        // slice too short is lengthened to exactly that by moving the marker that ends it (later;
        // for the last slice, the last marker earlier), keeping its play count, so a stutter or
        // a vamp stays one; only where the slice after (before) it leaves no room are the two
        // merged, and the merged slice plays as the longer of the two did (the later one on a
        // tie), so a short loop is never spread over a long neighbour. Without an end time the
        // last slice cannot be checked (the file's length is not in the model).
        let mut moved = Vec::new();
        let mut short = Vec::new();
        let mut marks: Vec<SliceMarker> = std::mem::take(slices);
        let mut kept: Vec<SliceMarker> = Vec::with_capacity(marks.len());
        let mut prev = start;
        let mut i = 0;
        while let Some(&s) = marks.get(i) {
            i += 1;
            if !slice_too_short(s.time - prev) {
                prev = s.time;
                kept.push(s);
                continue;
            }
            let next = marks.get(i).map(|n| n.time).or(end);
            let longer = prev + MIN_SLICE_SECONDS;
            if longer <= MAX_TIME && next.is_none_or(|n| !slice_too_short(n - longer)) {
                moved.push((s.time, longer));
                prev = longer;
                kept.push(SliceMarker { time: longer, ..s });
                continue;
            }
            // No room: merge with the next slice (`next` is known here).
            let next_len = next.map_or(f64::INFINITY, |n| n - s.time);
            short.push(s.time);
            let take_short = s.time - prev > next_len;
            match marks.get_mut(i) {
                Some(n) if take_short => n.play_count = s.play_count,
                Some(_) => {}
                None if take_short => *last_play_count = s.play_count,
                None => {}
            }
        }
        if let Some(end) = end {
            while let Some(&last) = kept.last()
                && slice_too_short(end - last.time)
            {
                let before = kept.len().checked_sub(2).and_then(|k| kept.get(k));
                let prev = before.map_or(start, |b| b.time);
                let earlier = end - MIN_SLICE_SECONDS;
                if !slice_too_short(earlier - prev) {
                    moved.push((last.time, earlier));
                    if let Some(l) = kept.last_mut() {
                        l.time = earlier;
                    }
                    break;
                }
                short.push(last.time);
                if last.time - prev > end - last.time {
                    *last_play_count = last.play_count;
                }
                kept.pop();
            }
        }
        *slices = kept;
        for (from, to) in moved {
            self.warn(id, IssueKind::SliceMoved { from, to });
        }
        for time in short {
            self.warn(id, IssueKind::SliceTooShort { time });
        }
    }

    fn cue_levels(&mut self, id: Option<CueId>, levels: &mut CueLevels, patch_outputs: usize) {
        let max = MAX_CHANNELS as usize;
        self.level(id, "levels.master_db", &mut levels.master_db);
        let extra = levels.outputs.len().saturating_sub(max);
        levels.outputs.truncate(max);
        self.count(id, "levels.outputs", extra);
        // Outputs the patch lacks are kept (silent at runtime), so shrinking a patch by mistake
        // and restoring it later loses no routing.
        let unpatched = levels.outputs.len().saturating_sub(patch_outputs);
        if unpatched > 0 {
            self.warn(id, IssueKind::UnpatchedOutputs { count: unpatched });
        }
        for output in &mut levels.outputs {
            self.level(id, "levels.outputs.db", &mut output.db);
        }
        let dropped = retain_counted(&mut levels.crosspoints, |c| {
            c.input < MAX_CHANNELS && c.output < MAX_CHANNELS
        });
        self.count(id, "levels.crosspoints", dropped);
        let duplicates = dedupe(&mut levels.crosspoints, |c| (c.input, c.output));
        self.duplicates(id, "levels.crosspoints", duplicates);
        for crosspoint in &mut levels.crosspoints {
            self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
        }
        let (mut dropped, mut seen, mut duplicates) = (0, HashSet::new(), 0);
        for gang in &mut levels.gangs {
            dropped += retain_counted(gang, |&i| i < MAX_CHANNELS);
            // An output belongs to at most one gang; the first wins.
            duplicates += retain_counted(gang, |&i| seen.insert(i));
        }
        levels.gangs.retain(|gang| !gang.is_empty());
        self.count(id, "levels.gangs", dropped);
        self.duplicates(id, "levels.gangs", duplicates);
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
        let duplicates = dedupe(&mut levels.crosspoints, |c| (c.input, c.output));
        self.duplicates(id, "levels.crosspoints", duplicates);
        for crosspoint in &mut levels.crosspoints {
            self.level(id, "levels.crosspoints.db", &mut crosspoint.db);
        }
    }

    /// Warns about each odd name or host kind; one warning per field kind, not per item, so a
    /// hostile file cannot flood the list. Nothing is changed.
    fn names(&mut self, workspace: &Workspace) {
        fn bad<'a>(mut names: impl Iterator<Item = &'a str>) -> bool {
            names.any(|n| name_problem(n).is_some())
        }
        let fields = [
            ("name", bad(std::iter::once(workspace.name.as_str()))),
            (
                "cue_lists.name",
                bad(workspace.cue_lists.iter().map(|l| l.name.as_str())),
            ),
            (
                "patches.name",
                bad(workspace.patches.iter().map(|p| p.name.as_str())),
            ),
            (
                "network_patches.name",
                bad(workspace.network_patches.iter().map(|p| p.name.as_str())),
            ),
            (
                "network_patches.host",
                workspace
                    .network_patches
                    .iter()
                    .any(|p| host_problem(&p.host).is_some()),
            ),
        ];
        for (field, odd) in fields {
            if odd {
                self.warn(None, IssueKind::InvalidName { field });
            }
        }
    }

    fn references(&mut self, workspace: &Workspace) {
        let mut by_id: HashMap<CueId, &Cue> = HashMap::new();
        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                by_id.entry(cue.id).or_insert(cue);
            });
        }
        let cycles = TriggerGraph::new(workspace);
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
                if cue.second_trigger.effective(&cue.kind) != cue.second_trigger {
                    self.warn(id, IssueKind::SecondTriggerNeedsPlaylist);
                }
                if let Some(action) = (cue.triggers.hotkey.as_ref())
                    .and_then(|key| workspace.settings.hotkeys.action_of(key))
                {
                    self.warn(id, IssueKind::HotkeyUsedByWorkspace { action });
                }
                if let Some(target) = starts(&cue.kind)
                    && target != cue.id
                    && cycles.component(cue.id).is_some()
                    && cycles.component(cue.id) == cycles.component(target)
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

/// What starts what: Start/GoTo-and-GO cues start their target, and a group may start any of
/// its children (whatever the mode, to stay on the safe side). A triggering cue is in a cycle
/// exactly when it shares a strongly connected component with its target.
struct TriggerGraph {
    component: HashMap<CueId, usize>,
}

impl TriggerGraph {
    fn new(workspace: &Workspace) -> Self {
        let mut cues: Vec<&Cue> = Vec::new();
        let mut node: HashMap<CueId, usize> = HashMap::new();
        for list in &workspace.cue_lists {
            visit(&list.cues, &mut |cue| {
                if let Entry::Vacant(entry) = node.entry(cue.id) {
                    entry.insert(cues.len());
                    cues.push(cue);
                }
            });
        }
        let successors: Vec<Vec<usize>> = cues
            .iter()
            .map(|cue| {
                let children = cue.children().iter().map(|c| c.id);
                children
                    .chain(starts(&cue.kind))
                    .filter_map(|id| node.get(&id).copied())
                    .collect()
            })
            .collect();
        let components = strongly_connected(&successors).unwrap_or_default();
        let component = node
            .into_iter()
            .filter_map(|(id, n)| Some((id, *components.get(n)?)))
            .collect();
        Self { component }
    }

    fn component(&self, id: CueId) -> Option<usize> {
        self.component.get(&id).copied()
    }
}

/// Tarjan's algorithm, iterative so a long chain cannot overflow the stack. O(nodes + edges).
/// Returns each node's component number; `None` only if `successors` names a node that does not
/// exist, which the graph built above never does.
fn strongly_connected(successors: &[Vec<usize>]) -> Option<Vec<usize>> {
    let n = successors.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut component = vec![0; n];
    let mut stack = Vec::new();
    // (node, next edge to follow); edge 0 means the node is being entered.
    let mut work: Vec<(usize, usize)> = Vec::new();
    let (mut next_index, mut next_component) = (0, 0);
    for root in 0..n {
        if index.get(root)?.is_some() {
            continue;
        }
        work.push((root, 0));
        while let Some((v, edge)) = work.pop() {
            if edge == 0 {
                *index.get_mut(v)? = Some(next_index);
                *low.get_mut(v)? = next_index;
                next_index += 1;
                stack.push(v);
                *on_stack.get_mut(v)? = true;
            }
            if let Some(&w) = successors.get(v)?.get(edge) {
                work.push((v, edge + 1));
                match *index.get(w)? {
                    None => work.push((w, 0)),
                    Some(w_index) if *on_stack.get(w)? => {
                        let v_low = low.get_mut(v)?;
                        *v_low = (*v_low).min(w_index);
                    }
                    Some(_) => {}
                }
                continue;
            }
            let v_low = *low.get(v)?;
            if Some(v_low) == *index.get(v)? {
                loop {
                    let w = stack.pop()?;
                    *on_stack.get_mut(w)? = false;
                    *component.get_mut(w)? = next_component;
                    if w == v {
                        break;
                    }
                }
                next_component += 1;
            }
            if let Some(&(parent, _)) = work.last() {
                let parent_low = low.get_mut(parent)?;
                *parent_low = (*parent_low).min(v_low);
            }
        }
    }
    Some(component)
}

/// Fades act on audio levels (a Group fades its children); Devamp needs slices or a playlist
/// (which it lets finish its current child). Other control cues accept any target.
pub(crate) fn can_target(source: &CueKind, target: &CueKind) -> bool {
    match source {
        CueKind::Fade { .. } => matches!(target, CueKind::Audio { .. } | CueKind::Group { .. }),
        CueKind::Devamp { .. } => matches!(
            target,
            CueKind::Audio { .. }
                | CueKind::Group {
                    mode: GroupMode::Playlist { .. },
                    ..
                }
        ),
        _ => true,
    }
}

/// Calls `f` on every cue in `cues`, parents first ([`crate::descendants`]).
pub(crate) fn visit<'a>(cues: &'a [Cue], f: &mut impl FnMut(&'a Cue)) {
    crate::tree::descendants(cues).for_each(f);
}
