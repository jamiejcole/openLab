// SPDX-License-Identifier: GPL-3.0-or-later

//! Cue numbers (ED-08): the next auto number, `SetCueNumbers` and `Renumber`. Same rules as
//! every command (see `command.rs`).

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use crate::model::{CueId, Workspace};
use crate::tree::walk_mut;
use crate::validate::{MAX_AUTO_NUMBER, visit};

use super::{Command, CommandError, Inverse, NumberAssignment, no_change};

/// Largest cue number auto-numbering reads from existing cues; larger ones are ignored.
const NUMBERING_LIMIT: f64 = 1e12;

// ---------------------------------------------------------------------------------------------
// Numbers

/// The number a cue created now gets (ED-08): one step above the highest numeric cue number in
/// the workspace, but not below `auto_number_start`.
pub fn next_auto_number(ws: &Workspace) -> Result<String, CommandError> {
    let settings = &ws.settings;
    let (start, increment) = (settings.auto_number_start, settings.auto_number_increment);
    check_numbering(start, increment)?;
    let mut highest: Option<f64> = None;
    let mut used = HashSet::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            let Some(number) = &cue.number else { return };
            used.insert(number.as_str());
            if let Ok(value) = number.trim().parse::<f64>()
                && value.is_finite()
                && (0.0..=NUMBERING_LIMIT).contains(&value)
            {
                highest = Some(highest.map_or(value, |h| h.max(value)));
            }
        });
    }
    let next = highest.map_or(start, |h| start.max(h + increment));
    let text = format_number(next);
    if used.contains(text.as_str()) {
        return Err(CommandError::InvalidNumbering(
            "next number is already used",
        ));
    }
    Ok(text)
}

fn check_numbering(start: f64, increment: f64) -> Result<(), CommandError> {
    if !(start.is_finite() && (0.0..=MAX_AUTO_NUMBER).contains(&start)) {
        return Err(CommandError::InvalidNumbering(
            "start must be between 0 and the maximum",
        ));
    }
    if !(increment.is_finite() && increment > 0.0 && increment <= MAX_AUTO_NUMBER) {
        return Err(CommandError::InvalidNumbering(
            "increment must be above 0 and at most the maximum",
        ));
    }
    Ok(())
}

/// Plain decimal text without float noise: 3 → "3", 0.1 + 0.2 → "0.3".
fn format_number(value: f64) -> String {
    // Adding 0.0 turns −0.0 into 0.0.
    let rounded = (value * 1e9).round() / 1e9 + 0.0;
    rounded.to_string()
}

pub(super) fn renumber(
    ws: &mut Workspace,
    ids: &[CueId],
    start: f64,
    increment: f64,
) -> Result<Inverse, CommandError> {
    check_numbering(start, increment)?;
    let wanted: HashSet<CueId> = ids.iter().copied().collect();
    let mut numbers = Vec::new();
    let mut found = HashSet::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            if wanted.contains(&cue.id) && found.insert(cue.id) {
                let step = numbers.len() as f64;
                numbers.push(NumberAssignment {
                    id: cue.id,
                    number: Some(format_number(start + step * increment)),
                });
            }
        });
    }
    if let Some(&missing) = ids.iter().find(|id| !found.contains(id)) {
        return Err(CommandError::CueNotFound(missing));
    }
    set_numbers(ws, numbers)
}

pub(super) fn set_numbers(
    ws: &mut Workspace,
    assignments: Vec<NumberAssignment>,
) -> Result<Inverse, CommandError> {
    let mut wanted: HashMap<CueId, Option<String>> = HashMap::new();
    for NumberAssignment { id, number } in assignments {
        if number.as_deref() == Some("") {
            return Err(CommandError::EmptyNumber(id));
        }
        if wanted.insert(id, number).is_some() {
            return Err(CommandError::DuplicateAssignment(id));
        }
    }

    let mut seen: HashMap<String, CueId> = HashMap::new();
    let mut found = HashSet::new();
    let mut old = Vec::new();
    let mut problem = None;
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            let number = match wanted.get(&cue.id) {
                Some(new) => {
                    found.insert(cue.id);
                    old.push(NumberAssignment {
                        id: cue.id,
                        number: cue.number.clone(),
                    });
                    new
                }
                None => &cue.number,
            };
            if let Some(number) = number
                && problem.is_none()
            {
                match seen.entry(number.clone()) {
                    Entry::Occupied(first) => {
                        problem = Some(CommandError::DuplicateNumber {
                            number: number.clone(),
                            used_by: *first.get(),
                        });
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(cue.id);
                    }
                }
            }
        });
    }
    if let Some(missing) = wanted.keys().find(|id| !found.contains(id)) {
        return Err(CommandError::CueNotFound(*missing));
    }
    if let Some(error) = problem {
        return Err(error);
    }

    if old.iter().all(|o| wanted.get(&o.id) == Some(&o.number)) {
        return Ok(no_change());
    }
    for list in &mut ws.cue_lists {
        walk_mut(&mut list.cues, &mut |cue| {
            if let Some(number) = wanted.get(&cue.id) {
                cue.number.clone_from(number);
            }
        });
    }
    Ok(Command::SetCueNumbers { numbers: old })
}
