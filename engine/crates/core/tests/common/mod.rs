// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

//! Builders for small test workspaces.

use openlab_core::*;
use uuid::Uuid;

pub fn cue_id(n: u128) -> CueId {
    CueId(Uuid::from_u128(n))
}

pub fn patch_id(n: u128) -> PatchId {
    PatchId(Uuid::from_u128(n))
}

pub fn net_id(n: u128) -> NetworkPatchId {
    NetworkPatchId(Uuid::from_u128(n))
}

pub fn list_id(n: u128) -> CueListId {
    CueListId(Uuid::from_u128(n))
}

pub fn cue(n: u128, kind: CueKind) -> Cue {
    Cue {
        id: cue_id(n),
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
        kind,
    }
}

pub fn numbered(n: u128, number: &str, kind: CueKind) -> Cue {
    Cue {
        number: Some(number.into()),
        ..cue(n, kind)
    }
}

pub fn no_fade() -> FadeSpec {
    FadeSpec {
        duration: 0.0,
        curve: FadeCurve::Linear,
    }
}

/// Audio cue on patch 1 (which [`workspace`] provides).
pub fn audio() -> CueKind {
    CueKind::Audio {
        media: Some(MediaRef::new("media/a.wav")),
        patch: patch_id(1),
        start_time: 0.0,
        end_time: None,
        fade_in: no_fade(),
        fade_out: no_fade(),
        slices: vec![],
        last_slice_play_count: 1,
        levels: CueLevels {
            master_db: 0.0,
            outputs: vec![out(0.0), out(0.0)],
            crosspoints: vec![],
            gangs: vec![],
        },
        preload: false,
    }
}

pub fn fade(target: u128) -> CueKind {
    CueKind::Fade {
        target: Some(cue_id(target)),
        duration: 5.0,
        curve: FadeCurve::EqualPower,
        levels: FadeLevels {
            master_db: Some(SILENCE_DB),
            output_db: vec![],
            crosspoints: vec![],
        },
        stop_target_when_done: true,
    }
}

pub fn out(db: f64) -> CueOutput {
    CueOutput { db, mute: false }
}

pub fn patch_output() -> PatchOutput {
    PatchOutput {
        level_db: 0.0,
        mute: false,
        delay_ms: 0.0,
        polarity_inverted: false,
    }
}

pub fn patch(n: u128, outputs: usize) -> OutputPatch {
    OutputPatch {
        id: patch_id(n),
        name: "Main".into(),
        master_db: 0.0,
        outputs: vec![patch_output(); outputs],
    }
}

pub fn group(children: Vec<Cue>) -> CueKind {
    CueKind::Group {
        mode: GroupMode::Timeline,
        children,
    }
}

/// One cue list (id 1) holding `cues`, output patch 1 and network patch 1.
pub fn workspace(cues: Vec<Cue>) -> Workspace {
    Workspace {
        id: WorkspaceId(Uuid::from_u128(1)),
        name: "Test".into(),
        settings: WorkspaceSettings {
            min_go_interval: 0.0,
            panic_duration: 5.0,
            autosave_interval: 30.0,
            osc_passcode: None,
            master_db: 0.0,
            hotkeys: TransportHotkeys::default(),
            auto_number_start: 1.0,
            auto_number_increment: 1.0,
        },
        patches: vec![patch(1, 2)],
        network_patches: vec![NetworkPatch {
            id: net_id(1),
            name: "Desk".into(),
            host: "127.0.0.1".into(),
            port: 53000,
            protocol: NetworkProtocol::Udp,
        }],
        cue_lists: vec![CueList {
            id: list_id(1),
            name: "Main".into(),
            cues,
        }],
    }
}

/// A current-version show file containing `workspace` exactly as given, bypassing the
/// checks in `save_show`.
pub fn show_text(workspace: &Workspace) -> String {
    format!(
        "{{\"format_version\": {FORMAT_VERSION}, \"workspace\": {}}}",
        serde_json::to_string(workspace).unwrap()
    )
}
