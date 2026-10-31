// SPDX-License-Identifier: GPL-3.0-or-later

//! Cue property commands (`SetCueProperty`, `BatchSet`; ED-04): reading, checking and
//! writing one [`CueProperty`] of a cue. Same rules as every command (see `command.rs`).

use crate::cue::{ContinueMode, Cue, CueColor, CueKind, OscArg, SecondTrigger, Triggers};
use crate::model::{CueId, Workspace};
use crate::network::MessageIssue;
use crate::tree::{cue_at, cue_at_mut, find_path};
use crate::validate::{
    IssueKind, MAX_NETWORK_ADDRESS_BYTES, MAX_NETWORK_ARGS, MAX_NETWORK_STRING_BYTES,
    subtree_issues,
};

use super::{Command, CommandError, CueProperty, Inverse, no_change, refuse};

// ---------------------------------------------------------------------------------------------
// Properties

impl CueProperty {
    /// The same property with the value `cue` has now, or `None` if `cue` cannot have it.
    fn current(&self, cue: &Cue) -> Option<Self> {
        Some(match self {
            Self::Name { .. } => Self::Name {
                value: cue.name.clone(),
            },
            Self::Notes { .. } => Self::Notes {
                value: cue.notes.clone(),
            },
            Self::Color { .. } => Self::Color { value: cue.color },
            Self::Flagged { .. } => Self::Flagged { value: cue.flagged },
            Self::Armed { .. } => Self::Armed { value: cue.armed },
            Self::PreWait { .. } => Self::PreWait {
                value: cue.pre_wait,
            },
            Self::PostWait { .. } => Self::PostWait {
                value: cue.post_wait,
            },
            Self::ContinueMode { .. } => Self::ContinueMode {
                value: cue.continue_mode,
            },
            Self::Triggers { .. } => Self::Triggers {
                value: cue.triggers.clone(),
            },
            Self::SecondTrigger { .. } => Self::SecondTrigger {
                value: cue.second_trigger,
            },
            Self::SecondTriggerOnRelease { .. } => Self::SecondTriggerOnRelease {
                value: cue.second_trigger_on_release,
            },
            Self::Target { .. } => {
                if !cue.kind.has_target() {
                    return None;
                }
                Self::Target {
                    value: cue.kind.target(),
                }
            }
            Self::GroupMode { .. } => match &cue.kind {
                CueKind::Group { mode, .. } => Self::GroupMode { value: *mode },
                _ => return None,
            },
            Self::Kind { .. } | Self::KindFields { .. } => Self::Kind {
                value: cue.kind.clone(),
            },
            Self::Media { .. } => match &cue.kind {
                CueKind::Audio { media, .. } => Self::Media {
                    value: media.clone(),
                },
                _ => return None,
            },
            Self::Destination { .. } => match &cue.kind {
                CueKind::Network { destination, .. } => Self::Destination {
                    value: *destination,
                },
                _ => return None,
            },
            Self::Address { .. } => match &cue.kind {
                CueKind::Network { address, .. } => Self::Address {
                    value: address.clone(),
                },
                _ => return None,
            },
            Self::Args { .. } => match &cue.kind {
                CueKind::Network { args, .. } => Self::Args {
                    value: args.clone(),
                },
                _ => return None,
            },
            Self::Duration { .. } => match &cue.kind {
                CueKind::Wait { duration } | CueKind::Fade { duration, .. } => {
                    Self::Duration { value: *duration }
                }
                _ => return None,
            },
            Self::LoadToTime { .. } => match &cue.kind {
                CueKind::Load { load_to_time, .. } => Self::LoadToTime {
                    value: *load_to_time,
                },
                _ => return None,
            },
        })
    }

    /// Whether `other` sets the same field (`History` coalescing).
    pub(crate) fn same_slot(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Sets the property. Returns `false`, changing nothing, if `cue` cannot have it.
    fn write(&self, cue: &mut Cue) -> bool {
        match self {
            Self::Name { value } => cue.name.clone_from(value),
            Self::Notes { value } => cue.notes.clone_from(value),
            Self::Color { value } => cue.color = *value,
            Self::Flagged { value } => cue.flagged = *value,
            Self::Armed { value } => cue.armed = *value,
            Self::PreWait { value } => cue.pre_wait = *value,
            Self::PostWait { value } => cue.post_wait = *value,
            Self::ContinueMode { value } => cue.continue_mode = *value,
            Self::Triggers { value } => cue.triggers.clone_from(value),
            Self::SecondTrigger { value } => cue.second_trigger = *value,
            Self::SecondTriggerOnRelease { value } => cue.second_trigger_on_release = *value,
            Self::Target { value } => return cue.kind.set_target(*value),
            Self::GroupMode { value } => match &mut cue.kind {
                CueKind::Group { mode, .. } => *mode = *value,
                _ => return false,
            },
            Self::Kind { value } => cue.kind = value.clone(),
            // Resolved to a `Kind` by `set_property` before anything is written.
            Self::KindFields { .. } => return false,
            Self::Media { value } => match &mut cue.kind {
                CueKind::Audio { media, .. } => media.clone_from(value),
                _ => return false,
            },
            Self::Destination { value } => match &mut cue.kind {
                CueKind::Network { destination, .. } => *destination = *value,
                _ => return false,
            },
            Self::Address { value } => match &mut cue.kind {
                CueKind::Network { address, .. } => address.clone_from(value),
                _ => return false,
            },
            Self::Args { value } => match &mut cue.kind {
                CueKind::Network { args, .. } => args.clone_from(value),
                _ => return false,
            },
            Self::Duration { value } => match &mut cue.kind {
                CueKind::Wait { duration } | CueKind::Fade { duration, .. } => *duration = *value,
                _ => return false,
            },
            Self::LoadToTime { value } => match &mut cue.kind {
                CueKind::Load { load_to_time, .. } => *load_to_time = *value,
                _ => return false,
            },
        }
        true
    }
}

/// A cue with every field valid, to check one new value on its own.
/// The size [`IssueKind::TooLarge`] bounds for `field` of the one cue in `cue` (bytes; entries
/// for `args`; the longest string argument for `args.value`).
fn sized(cue: &[Cue; 1], field: &str) -> usize {
    let [cue] = cue;
    let (address, args) = match &cue.kind {
        CueKind::Network { address, args, .. } => (address.len(), args.as_slice()),
        _ => (0, &[][..]),
    };
    match field {
        "name" => cue.name.len(),
        "notes" => cue.notes.len(),
        "address" => address,
        "args" => args.len(),
        "args.value" => args
            .iter()
            .map(|a| match a {
                OscArg::String { value } => value.len(),
                _ => 0,
            })
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

fn blank_cue(id: CueId) -> Cue {
    Cue {
        id,
        number: None,
        name: String::new(),
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

/// The cue's kind with `fields` set, every other field as it is now.
fn merge_kind_fields(
    id: CueId,
    kind: &CueKind,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<CueKind, CommandError> {
    let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(kind) else {
        return Err(CommandError::Internal);
    };
    for (key, value) in fields {
        if matches!(key.as_str(), "type" | "children" | "media") {
            return Err(CommandError::InvalidValue(
                "that kind field cannot be set on its own",
            ));
        }
        let Some(slot) = merged.get_mut(key) else {
            return Err(CommandError::PropertyNotApplicable(id));
        };
        *slot = value.clone();
    }
    serde_json::from_value(serde_json::Value::Object(merged))
        .map_err(|_| CommandError::InvalidValue("a kind field has a value of the wrong type"))
}

pub(super) fn set_property(
    ws: &mut Workspace,
    id: CueId,
    property: CueProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
    let cue = cue_at(ws, &path).ok_or(CommandError::Internal)?;
    // Judged and applied as the whole new kind, against the kind the cue has now.
    let property = match property {
        CueProperty::KindFields { value } => {
            if matches!(cue.kind, CueKind::Group { .. }) {
                return Err(CommandError::KindChangeInvolvesGroup(id));
            }
            CueProperty::Kind {
                value: merge_kind_fields(id, &cue.kind, &value)?,
            }
        }
        other => other,
    };
    if let CueProperty::Kind { value } = &property
        && (matches!(cue.kind, CueKind::Group { .. }) || matches!(value, CueKind::Group { .. }))
    {
        return Err(CommandError::KindChangeInvolvesGroup(id));
    }
    let old = property
        .current(cue)
        .ok_or(CommandError::PropertyNotApplicable(id))?;

    // The new value has to be valid by itself. Judging it inside the cue would excuse a bad
    // value whenever the cue already had a problem in the same field.
    // A value inside the kind is judged in a copy of the cue's kind, so the probe has the field.
    let in_kind = matches!(
        property,
        CueProperty::Duration { .. } | CueProperty::LoadToTime { .. }
    );
    let blank = || {
        let mut c = blank_cue(id);
        if in_kind {
            c.kind = cue.kind.clone();
        }
        c
    };
    let mut probe = blank();
    if in_kind
        || matches!(
            property,
            CueProperty::Kind { .. }
                | CueProperty::Name { .. }
                | CueProperty::Notes { .. }
                | CueProperty::PreWait { .. }
                | CueProperty::PostWait { .. }
                | CueProperty::Triggers { .. }
        )
    {
        property.write(&mut probe);
        if !restoring {
            // Only what the new value introduces: the old one may carry a warning from load.
            let mut was = blank();
            old.write(&mut was);
            let was = [was];
            let probe = [probe];
            let existing = subtree_issues(ws, &was, path.indices.len());
            let issues = subtree_issues(ws, &probe, path.indices.len())
                .into_iter()
                .filter(|i| match i.kind {
                    // A loaded over-limit value may be replaced by one no longer than it.
                    IssueKind::TooLarge { field, .. } if existing.contains(i) => {
                        sized(&probe, field) > sized(&was, field)
                    }
                    _ => !existing.contains(i),
                })
                .collect();
            refuse(issues)?;
        }
    }

    if old == property {
        return Ok(no_change());
    }
    if !restoring {
        check_network_value(cue, &property)?;
    }
    let cue = cue_at_mut(ws, &path).ok_or(CommandError::Internal)?;
    if !property.write(cue) {
        return Err(CommandError::Internal);
    }
    Ok(Command::SetCueProperty { id, property: old })
}

/// A new Network address or argument list is accepted whatever it says (a destination that
/// names no patch, or none, too: the cue shows broken, `network_patch_missing`); only the
/// memory bounds ([`MAX_NETWORK_ADDRESS_BYTES`] and the others) are refused, as for a new or
/// replaced Network cue (`subtree_issues`). A non-finite float is also refused: JSON cannot hold
/// one, so the show would not save and load back (load repairs it to 0 with a warning).
fn check_network_value(cue: &Cue, property: &CueProperty) -> Result<(), CommandError> {
    if !matches!(cue.kind, CueKind::Network { .. }) {
        return Ok(());
    }
    let issue = match property {
        CueProperty::Address { value } if value.len() > MAX_NETWORK_ADDRESS_BYTES => {
            MessageIssue::AddressTooLong
        }
        CueProperty::Args { value } if value.len() > MAX_NETWORK_ARGS => MessageIssue::TooManyArgs,
        CueProperty::Args { value }
            if value
                .iter()
                .any(|a| matches!(a, OscArg::Float { value } if !value.is_finite())) =>
        {
            MessageIssue::FloatNotFinite
        }
        CueProperty::Args { value }
            if value.iter().any(
                |a| matches!(a, OscArg::String { value } if value.len() > MAX_NETWORK_STRING_BYTES),
            ) =>
        {
            MessageIssue::StringTooLong
        }
        _ => return Ok(()),
    };
    Err(CommandError::InvalidMessage(issue))
}

/// `BatchSet`: applies the property in place, one cue at a time, keeping each cue's old value.
/// If a cue is refused, the cues already written get their old values back (in reverse, so a
/// repeated id also ends as it began). The cost follows the cues touched, not the show; the
/// workspace is not cloned. The inverse is the same flat list of `SetCueProperty` a
/// `Transaction` of the sets would give.
pub(super) fn batch_set(
    ws: &mut Workspace,
    ids: Vec<CueId>,
    property: &CueProperty,
    restoring: bool,
) -> Result<Inverse, CommandError> {
    let mut undo_steps: Vec<Inverse> = Vec::with_capacity(ids.len());
    for (index, id) in ids.into_iter().enumerate() {
        match set_property(ws, id, property.clone(), restoring) {
            Ok(inverse) => undo_steps.push(inverse),
            Err(error) => {
                // Each inverse is a plain write of a value the cue held, so it cannot be
                // refused; `restoring` skips validation for the same reason.
                while let Some(step) = undo_steps.pop() {
                    if let Command::SetCueProperty { id, property } = step {
                        let _ = set_property(ws, id, property, true);
                    }
                }
                return Err(CommandError::InTransaction {
                    index,
                    source: Box::new(error),
                });
            }
        }
    }
    undo_steps.reverse();
    Ok(Command::Transaction {
        commands: undo_steps
            .into_iter()
            .flat_map(|step| match step {
                Command::Transaction { commands } => commands,
                other => vec![other],
            })
            .collect(),
    })
}
