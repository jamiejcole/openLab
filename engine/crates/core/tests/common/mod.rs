// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

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
        second_trigger: SecondTrigger::Nothing,
        second_trigger_on_release: false,
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
            osc_access: openlab_core::OscAccess::default(),
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

/// Calls `handle` as the binary must: `model_changed` before the first call
/// after any change to the workspace. Checks that the engine never caught an unsignalled change.
pub fn handle_as_binary(
    engine: &mut CueEngine,
    ws: &Workspace,
    last: &mut Option<Workspace>,
    input: Input,
    now: SampleTime,
) -> Vec<Action> {
    if last.as_ref() != Some(ws) {
        engine.model_changed();
        *last = Some(ws.clone());
    }
    let out = engine.handle(ws, input, now);
    assert_eq!(engine.unsignalled_model_changes(), 0);
    out
}

// ---- Cue-engine rig, shared by the `cue_engine*` test files ----

/// Sample rate of every cue-engine test: one frame is one millisecond.
pub const RATE: u32 = 1000;

pub fn id(n: u128) -> CueId {
    cue_id(n)
}

/// The cue list [`workspace`] builds.
pub fn list() -> CueListId {
    list_id(1)
}

pub fn memo(n: u128) -> Cue {
    cue(n, CueKind::Memo)
}

pub fn wait(n: u128, seconds: f64) -> Cue {
    cue(n, CueKind::Wait { duration: seconds })
}

pub fn sound(n: u128) -> Cue {
    cue(n, audio())
}

/// Sets pre-wait, post-wait and continue mode.
pub fn timed(mut c: Cue, pre: f64, post: f64, mode: ContinueMode) -> Cue {
    c.pre_wait = pre;
    c.post_wait = post;
    c.continue_mode = mode;
    c
}

pub fn changes(actions: &[Action], n: u128) -> Vec<(CueState, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged { cue, state, at } if *cue == id(n) => Some((*state, *at)),
            _ => None,
        })
        .collect()
}

pub fn warnings(actions: &[Action]) -> Vec<EngineWarning> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Warning(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

pub fn starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StateChanged {
                cue,
                state: CueState::PreWaiting,
                at,
            } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

pub fn voice_starts(actions: &[Action]) -> Vec<(CueId, SampleTime)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StartVoice { cue, at, .. } => Some((*cue, *at)),
            _ => None,
        })
        .collect()
}

pub fn voice_stops(actions: &[Action]) -> Vec<(CueId, SampleTime, Option<StopFade>)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::StopVoice { cue, at, fade, .. } => Some((*cue, *at, *fade)),
            _ => None,
        })
        .collect()
}

/// A workspace and a `CueEngine` driven the way the binary drives them (`handle_as_binary`),
/// with a helper per input. Files add their own helpers in an `impl Rig` block.
pub struct Rig {
    pub ws: Workspace,
    pub engine: CueEngine,
    /// The workspace as the engine last saw it (see `handle_as_binary`).
    pub last: Option<Workspace>,
    /// Also check the engine's problem cache against a rebuild after every input.
    pub check_cache: bool,
}

impl Rig {
    /// One list holding `cues`, no lookahead, seed 1, a tick at 0.
    pub fn new(cues: Vec<Cue>) -> Self {
        Self::with_lookahead(cues, 0)
    }

    pub fn with_lookahead(cues: Vec<Cue>, lookahead: SampleTime) -> Self {
        Self::with_config(cues, EngineConfig::new(RATE, lookahead, 1).unwrap())
    }

    pub fn seeded(cues: Vec<Cue>, seed: u64) -> Self {
        Self::with_config(cues, EngineConfig::new(RATE, 0, seed).unwrap())
    }

    pub fn with_config(cues: Vec<Cue>, config: EngineConfig) -> Self {
        Self::on(workspace(cues), config)
    }

    /// Like `new`, and the problem cache is checked after every input.
    pub fn checked(cues: Vec<Cue>) -> Self {
        let mut rig = Self::new(cues);
        rig.check_cache = true;
        rig
    }

    /// Like `new`, with a one-second panic fade (the default is five).
    pub fn panicking(cues: Vec<Cue>) -> Self {
        Self::panicking_with(cues, 0)
    }

    pub fn panicking_with(cues: Vec<Cue>, lookahead: SampleTime) -> Self {
        let mut ws = workspace(cues);
        ws.settings.panic_duration = 1.0;
        Self::on(ws, EngineConfig::new(RATE, lookahead, 1).unwrap())
    }

    pub fn on(ws: Workspace, config: EngineConfig) -> Self {
        let mut rig = Self {
            ws,
            engine: CueEngine::new(config),
            last: None,
            check_cache: false,
        };
        rig.tick(0);
        rig
    }

    pub fn at(&mut self, now: SampleTime, input: Input) -> Vec<Action> {
        let out = handle_as_binary(&mut self.engine, &self.ws, &mut self.last, input, now);
        if self.check_cache {
            self.engine.verify_problem_cache(&self.ws).unwrap();
        }
        out
    }

    pub fn tick(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Tick)
    }

    pub fn go(&mut self, now: SampleTime) -> Vec<Action> {
        self.at(now, Input::Go { list: list() })
    }

    pub fn start(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Start { cue: id(n) })
    }

    pub fn pause(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Pause { cue: id(n) })
    }

    pub fn resume(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Resume { cue: id(n) })
    }

    pub fn stop(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(
            now,
            Input::Stop {
                cue: id(n),
                fade_out: None,
            },
        )
    }

    pub fn ended(&mut self, now: SampleTime, n: u128, at: SampleTime) -> Vec<Action> {
        let run = self.run(n);
        self.at(
            now,
            Input::ActionEnded {
                cue: id(n),
                run,
                at,
            },
        )
    }

    pub fn failed(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        let run = self.run(n);
        self.at(
            now,
            Input::ActionFailed {
                cue: id(n),
                run,
                reason: BrokenReason::StreamFailed,
            },
        )
    }

    pub fn set_playhead(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(
            now,
            Input::SetPlayhead {
                list: list(),
                cue: Some(id(n)),
            },
        )
    }

    pub fn run(&self, n: u128) -> RunId {
        self.engine.run_of(id(n)).unwrap()
    }

    pub fn state(&self, n: u128) -> CueState {
        self.engine.state(id(n))
    }

    pub fn playhead(&self) -> Option<CueId> {
        self.engine.playhead(list())
    }

    pub fn release(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::TriggerReleased { cue: id(n) })
    }

    pub fn preview(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::Preview { cue: id(n) })
    }

    pub fn stop_preview(&mut self, now: SampleTime, n: u128) -> Vec<Action> {
        self.at(now, Input::StopPreview { cue: Some(id(n)) })
    }

    pub fn problems(&self, n: u128) -> Vec<CueProblem> {
        self.engine.problems(id(n)).to_vec()
    }

    pub fn level(&self, n: u128, param: LevelParam, at: SampleTime) -> Option<f32> {
        self.engine.fade_level(id(n), param, at)
    }

    pub fn cues_mut(&mut self) -> &mut Vec<Cue> {
        &mut self.ws.cue_lists[0].cues
    }

    /// A cue (or group child) of the first list, to edit in place.
    pub fn find_mut(&mut self, n: u128) -> &mut Cue {
        fn find(cues: &mut [Cue], id: CueId) -> Option<&mut Cue> {
            for c in cues {
                if c.id == id {
                    return Some(c);
                }
                if let CueKind::Group { children, .. } = &mut c.kind
                    && let Some(found) = find(children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&mut self.ws.cue_lists[0].cues, id(n)).unwrap()
    }
}
