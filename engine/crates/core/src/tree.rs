// SPDX-License-Identifier: GPL-3.0-or-later

//! Navigation inside the cue tree by index path, so commands can find, take and insert cues
//! without indexing that could panic.

use crate::cue::{Cue, CueKind};
use crate::model::{CueId, Workspace};

/// Where a cue is: a cue list (by position) and the index at each level of nesting. Never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CuePath {
    pub list: usize,
    pub indices: Vec<usize>,
}

impl CuePath {
    /// Path of the container holding this cue and the cue's index within it.
    pub fn split(&self) -> Option<(&[usize], usize)> {
        let (last, parents) = self.indices.split_last()?;
        Some((parents, *last))
    }
}

pub(crate) fn find_path(workspace: &Workspace, id: CueId) -> Option<CuePath> {
    workspace
        .cue_lists
        .iter()
        .enumerate()
        .find_map(|(list, l)| {
            let mut indices = Vec::new();
            search(&l.cues, id, &mut indices).then_some(CuePath { list, indices })
        })
}

fn search(cues: &[Cue], id: CueId, path: &mut Vec<usize>) -> bool {
    for (i, cue) in cues.iter().enumerate() {
        path.push(i);
        if cue.id == id || search(cue.children(), id, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// The cues directly inside the cue list `list` (no `parents`) or inside the group at `parents`.
pub(crate) fn container<'a>(
    workspace: &'a Workspace,
    list: usize,
    parents: &[usize],
) -> Option<&'a Vec<Cue>> {
    let mut cues = &workspace.cue_lists.get(list)?.cues;
    for &i in parents {
        cues = match &cues.get(i)?.kind {
            CueKind::Group { children, .. } => children,
            _ => return None,
        };
    }
    Some(cues)
}

pub(crate) fn container_mut<'a>(
    workspace: &'a mut Workspace,
    list: usize,
    parents: &[usize],
) -> Option<&'a mut Vec<Cue>> {
    let mut cues = &mut workspace.cue_lists.get_mut(list)?.cues;
    for &i in parents {
        cues = match &mut cues.get_mut(i)?.kind {
            CueKind::Group { children, .. } => children,
            _ => return None,
        };
    }
    Some(cues)
}

pub(crate) fn cue_at<'a>(workspace: &'a Workspace, path: &CuePath) -> Option<&'a Cue> {
    let (parents, index) = path.split()?;
    container(workspace, path.list, parents)?.get(index)
}

pub(crate) fn cue_at_mut<'a>(workspace: &'a mut Workspace, path: &CuePath) -> Option<&'a mut Cue> {
    let (parents, index) = path.split()?;
    container_mut(workspace, path.list, parents)?.get_mut(index)
}

/// Removes the cue at `index`, if there is one.
pub(crate) fn take(cues: &mut Vec<Cue>, index: usize) -> Option<Cue> {
    (index < cues.len()).then(|| cues.remove(index))
}

/// Inserts `new` so the first lands at `index`. Gives them back if `index` is past the end.
pub(crate) fn insert_all(cues: &mut Vec<Cue>, index: usize, new: Vec<Cue>) -> Result<(), Vec<Cue>> {
    if index > cues.len() {
        return Err(new);
    }
    cues.splice(index..index, new);
    Ok(())
}

/// Depth of the deepest cue in `cues` (1 for a flat list, 0 for none). Stops counting once it
/// passes `limit`, and is iterative, so a hostile tree cannot overflow the stack.
pub(crate) fn height(cues: &[Cue], limit: usize) -> usize {
    let mut deepest = 0;
    let mut work: Vec<(&[Cue], usize)> = vec![(cues, 1)];
    while let Some((level, depth)) = work.pop() {
        if level.is_empty() {
            continue;
        }
        deepest = deepest.max(depth);
        if deepest > limit {
            break;
        }
        work.extend(level.iter().map(|cue| (cue.children(), depth + 1)));
    }
    deepest
}

/// Calls `f` on every cue, parents before children. Recursive: bound the depth with [`height`]
/// first unless the tree is known to be valid.
pub(crate) fn walk_mut(cues: &mut [Cue], f: &mut impl FnMut(&mut Cue)) {
    for cue in cues {
        f(cue);
        if let CueKind::Group { children, .. } = &mut cue.kind {
            walk_mut(children, f);
        }
    }
}
