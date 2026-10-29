// SPDX-License-Identifier: GPL-3.0-or-later

//! Cue tree structure commands (ED-03, ED-05): add and paste, `RemoveCue`, `MoveCue` and
//! `ReparentCue`, and the positions they name. Same rules as every command (see `command.rs`).

use std::collections::{HashMap, HashSet};

use crate::cue::{Cue, CueKind};
use crate::model::{CueId, Workspace};
use crate::tree::{CuePath, container, container_mut, cue_at, find_path, height, insert_all, take};
use crate::validate::{MAX_GROUP_DEPTH, subtree_issues, visit};

use super::{Command, CommandError, Inverse, Position, refuse};

// ---------------------------------------------------------------------------------------------
// Positions

/// A resolved [`Position`]: the cue list's index and the path to the container group.
struct Container {
    list: usize,
    parents: Vec<usize>,
}

fn resolve(ws: &Workspace, at: &Position) -> Result<Container, CommandError> {
    let list = ws
        .cue_lists
        .iter()
        .position(|l| l.id == at.list)
        .ok_or(CommandError::CueListNotFound(at.list))?;
    let parents = match at.parent {
        None => Vec::new(),
        Some(parent) => {
            let path = find_path(ws, parent).ok_or(CommandError::CueNotFound(parent))?;
            if path.list != list {
                return Err(CommandError::ParentNotInList {
                    parent,
                    list: at.list,
                });
            }
            let cue = cue_at(ws, &path).ok_or(CommandError::Internal)?;
            if !matches!(cue.kind, CueKind::Group { .. }) {
                return Err(CommandError::NotAGroup(parent));
            }
            path.indices
        }
    };
    Ok(Container { list, parents })
}

pub(super) fn position_of(ws: &Workspace, path: &CuePath) -> Option<Position> {
    let (parents, index) = path.split()?;
    let parent = if parents.is_empty() {
        None
    } else {
        let parent_path = CuePath {
            list: path.list,
            indices: parents.to_vec(),
        };
        Some(cue_at(ws, &parent_path)?.id)
    };
    Some(Position {
        list: ws.cue_lists.get(path.list)?.id,
        parent,
        index,
    })
}

// ---------------------------------------------------------------------------------------------
// Add, paste, remove, move

/// With `restoring` (undo, redo, rollback: the cues are ones the workspace held), values are
/// not judged, as for a property set: a cue load kept with a warning must be able to come back.
/// Ids, numbers, positions and nesting still are.
pub(super) fn insert(
    ws: &mut Workspace,
    at: &Position,
    cues: Vec<Cue>,
    restoring: bool,
) -> Result<(), CommandError> {
    let dest = resolve(ws, at)?;
    let len = container(ws, dest.list, &dest.parents)
        .ok_or(CommandError::Internal)?
        .len();
    if at.index > len {
        return Err(CommandError::IndexOutOfRange {
            index: at.index,
            len,
        });
    }
    // Bound the nesting before anything recurses over the new cues.
    let depth = dest.parents.len() + 1;
    if depth - 1 + height(&cues, MAX_GROUP_DEPTH) > MAX_GROUP_DEPTH {
        return Err(CommandError::NestingTooDeep);
    }
    check_new_cues(ws, &cues, depth, restoring)?;
    let list = container_mut(ws, dest.list, &dest.parents).ok_or(CommandError::Internal)?;
    insert_all(list, at.index, cues).map_err(|_| CommandError::Internal)
}

/// Ids and numbers must be unique across the workspace and, unless `restoring`, values valid.
fn check_new_cues(
    ws: &Workspace,
    cues: &[Cue],
    depth: usize,
    restoring: bool,
) -> Result<(), CommandError> {
    let mut ids = HashSet::new();
    let mut numbers = HashMap::new();
    for list in &ws.cue_lists {
        visit(&list.cues, &mut |cue| {
            ids.insert(cue.id);
            if let Some(number) = &cue.number {
                numbers.insert(number.clone(), cue.id);
            }
        });
    }
    let mut problem = None;
    visit(cues, &mut |cue| {
        if problem.is_some() {
            return;
        }
        if !ids.insert(cue.id) {
            problem = Some(CommandError::DuplicateCueId(cue.id));
        } else if let Some(number) = &cue.number {
            if number.is_empty() {
                problem = Some(CommandError::EmptyNumber(cue.id));
            } else if let Some(&used_by) = numbers.get(number) {
                problem = Some(CommandError::DuplicateNumber {
                    number: number.clone(),
                    used_by,
                });
            } else {
                numbers.insert(number.clone(), cue.id);
            }
        }
    });
    if let Some(error) = problem {
        return Err(error);
    }
    if restoring {
        return Ok(());
    }
    refuse(subtree_issues(ws, cues, depth))
}

pub(super) fn remove(ws: &mut Workspace, id: CueId) -> Result<Inverse, CommandError> {
    let path = find_path(ws, id).ok_or(CommandError::CueNotFound(id))?;
    let at = position_of(ws, &path).ok_or(CommandError::Internal)?;
    let (parents, index) = path.split().ok_or(CommandError::Internal)?;
    let list = container_mut(ws, path.list, parents).ok_or(CommandError::Internal)?;
    let cue = take(list, index).ok_or(CommandError::Internal)?;
    Ok(Command::AddCue {
        at,
        cue,
        auto_number: false,
    })
}

pub(super) fn relocate(
    ws: &mut Workspace,
    id: CueId,
    path: &CuePath,
    to: &Position,
) -> Result<(), CommandError> {
    let dest = resolve(ws, to)?;
    if dest.list == path.list && dest.parents.starts_with(&path.indices) {
        return Err(CommandError::MoveIntoSelf(id));
    }
    let cue = cue_at(ws, path).ok_or(CommandError::Internal)?;
    let subtree = height(std::slice::from_ref(cue), MAX_GROUP_DEPTH);
    if dest.parents.len() + subtree > MAX_GROUP_DEPTH {
        return Err(CommandError::NestingTooDeep);
    }
    let (src_parents, src_index) = path.split().ok_or(CommandError::Internal)?;
    let len = container(ws, dest.list, &dest.parents)
        .ok_or(CommandError::Internal)?
        .len();
    let same = dest.list == path.list && dest.parents == src_parents;
    let len = if same { len.saturating_sub(1) } else { len };
    if to.index > len {
        return Err(CommandError::IndexOutOfRange {
            index: to.index,
            len,
        });
    }

    let source = container_mut(ws, path.list, src_parents).ok_or(CommandError::Internal)?;
    let cue = take(source, src_index).ok_or(CommandError::Internal)?;
    // Taking the cue out may have shifted the destination's path: resolve it again.
    let left_over = match resolve(ws, to)
        .ok()
        .and_then(|d| container_mut(ws, d.list, &d.parents))
    {
        Some(list) => insert_all(list, to.index, vec![cue]).err(),
        None => Some(vec![cue]),
    };
    if let Some(cue) = left_over {
        // Not expected after the checks above. Put the cue back; nothing else has changed.
        if let Some(list) = container_mut(ws, path.list, src_parents) {
            let _ = insert_all(list, src_index, cue);
        }
        return Err(CommandError::Internal);
    }
    Ok(())
}
