// SPDX-License-Identifier: GPL-3.0-or-later

//! What an undo step is called, so a client can name it in its Edit menu ("Undo Rename Output
//! Route"). The engine sends the name of the step the next undo and the next redo would apply
//! (`document`, docs/protocol.md); the client words it.

use serde::{Deserialize, Serialize};

use crate::command::{Command, CueProperty};
use crate::model::Workspace;
use crate::video::{RegionProperty, RouteProperty, StageProperty};

/// The name of one undo step. Steps of commands not listed here have none: the client then says
/// Undo or Redo alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepName {
    AddStage,
    RemoveStage,
    MoveStage,
    RenameStage,
    ResizeStage,
    StageKeepBlack,
    StageLayer,
    AddRoute,
    RemoveRoute,
    MoveRoute,
    RenameRoute,
    ResizeRoute,
    RotateRoute,
    RouteScaling,
    /// Several properties of one route changed together (its editor's OK).
    EditRoute,
    AddRegion,
    RemoveRegion,
    ResizeRegion,
    MoveRegion,
    RegionRoute,
    AddCue,
    RemoveCue,
    MoveCue,
    /// One or several cues pasted.
    PasteCues,
    RenumberCues,
    RenameCue,
    /// Any other property of one or several cues.
    EditCue,
}

impl StepName {
    /// A step of cue edits: several of them in one transaction are still one named step.
    fn is_cue_edit(self) -> bool {
        matches!(
            self,
            StepName::AddCue
                | StepName::RemoveCue
                | StepName::MoveCue
                | StepName::PasteCues
                | StepName::RenumberCues
                | StepName::RenameCue
                | StepName::EditCue
        )
    }
}

/// The name of the step `command` makes when applied to `workspace` as it is before the command
/// (a region's new bounds are a resize when its size changes, else a move).
pub fn step_name(command: &Command, workspace: &Workspace) -> Option<StepName> {
    Some(match command {
        Command::AddStage { .. } => StepName::AddStage,
        Command::RemoveStage { .. } => StepName::RemoveStage,
        Command::MoveStage { .. } => StepName::MoveStage,
        Command::SetStageProperty { property, .. } => match property {
            StageProperty::Name { .. } => StepName::RenameStage,
            StageProperty::Size { .. } => StepName::ResizeStage,
            StageProperty::KeepBlack { .. } => StepName::StageKeepBlack,
            StageProperty::Layer { .. } => StepName::StageLayer,
        },
        Command::AddCue { .. } => StepName::AddCue,
        Command::RemoveCue { .. } => StepName::RemoveCue,
        Command::MoveCue { .. } | Command::ReparentCue { .. } => StepName::MoveCue,
        Command::Paste { .. } | Command::PasteClipboard { .. } => StepName::PasteCues,
        Command::Renumber { .. } | Command::SetCueNumbers { .. } => StepName::RenumberCues,
        Command::SetCueProperty { property, .. } | Command::BatchSet { property, .. } => {
            match property {
                CueProperty::Name { .. } => StepName::RenameCue,
                _ => StepName::EditCue,
            }
        }
        Command::AddRoute { .. } => StepName::AddRoute,
        Command::RemoveRoute { .. } => StepName::RemoveRoute,
        Command::MoveRoute { .. } => StepName::MoveRoute,
        Command::SetRouteProperty { property, .. } => match property {
            RouteProperty::Name { .. } => StepName::RenameRoute,
            RouteProperty::Size { .. } => StepName::ResizeRoute,
            RouteProperty::Rotation { .. } => StepName::RotateRoute,
            RouteProperty::Scaling { .. } => StepName::RouteScaling,
            // No step of its own (`Command::is_view_aid`).
            RouteProperty::GuideColour { .. } => return None,
        },
        Command::AddRegion { .. } => StepName::AddRegion,
        Command::RemoveRegion { .. } => StepName::RemoveRegion,
        Command::SetRegionProperty {
            stage,
            id,
            property,
        } => match property {
            RegionProperty::Route { .. } => StepName::RegionRoute,
            RegionProperty::Bounds { width, height, .. } => {
                let old = workspace
                    .stages
                    .iter()
                    .find(|s| s.id == *stage)
                    .and_then(|s| s.regions.iter().find(|r| r.id == *id));
                match old {
                    Some(r) if (r.width, r.height) == (*width, *height) => StepName::MoveRegion,
                    _ => StepName::ResizeRegion,
                }
            }
        },
        // A transaction of one named command (as a client may send any edit) is named by it.
        Command::Transaction { commands } => match commands.as_slice() {
            [only] => return step_name(only, workspace),
            [Command::SetRouteProperty { id: first, .. }, rest @ ..]
                if rest
                    .iter()
                    .all(|c| matches!(c, Command::SetRouteProperty { id, .. } if id == first)) =>
            {
                StepName::EditRoute
            }
            _ => {
                // Several edits of cues that all mean the same thing (a delete of a selection).
                let mut names = commands.iter().map(|c| step_name(c, workspace));
                match names.next().flatten() {
                    Some(first) if first.is_cue_edit() && names.all(|n| n == Some(first)) => first,
                    _ => return None,
                }
            }
        },
        _ => return None,
    })
}
