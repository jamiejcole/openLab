// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The stage host draws with Metal on macOS only; elsewhere it has no video output (its own
// unit tests cover that).
#![cfg(target_os = "macos")]

//! Stages on the real engine (VID-01, VID-02, VID-12): the engine's control loop on the offline
//! driver runs the real stage host (`openlab-stage`, test-support build) offscreen on virtual
//! displays, and a test reads what a stage shows back from the GPU: the test card's
//! frame-number barcode and a few point colours. No window opens, nothing is compared with a
//! stored image, and waits are hang guards, not timings.

#[path = "../../engine/tests/show/mod.rs"]
mod show;

use std::path::PathBuf;
use std::time::Duration;

use openlab_core::protocol::{
    ClientMessage, ErrorCode, Outcome, Request, ResultData, ServerMessage, StageMappingEntry,
    TransportAction,
};
use openlab_core::stage_link::{Display, StageState};
use openlab_core::{Stage, StageId, StageMapping, Workspace};
use openlab_engine::control::ControlConfig;
use openlab_engine::control::offline::OfflineShow;
use openlab_engine::stage::StageHostConfig;
use show::*;
use uuid::Uuid;

const STAGE_HOST: &str = env!("CARGO_BIN_EXE_openlab-stage");

fn stage_id(n: u128) -> StageId {
    StageId(Uuid::from_u128(0x5_0000 + n))
}

fn stage(n: u128) -> Stage {
    Stage::new(stage_id(n), format!("Stage {n}"), 1920, 1080)
}

fn host_config(args: &[&str]) -> StageHostConfig {
    let mut c = StageHostConfig::new(PathBuf::from(STAGE_HOST));
    c.args = args.iter().map(|a| (*a).to_owned()).collect();
    c
}

/// Two virtual displays, drawn offscreen.
fn virtual_host() -> StageHostConfig {
    host_config(&["--virtual-displays", "V1:1920x1080,V2:1280x720"])
}

fn mapping(display: &str) -> StageMapping {
    StageMapping {
        display: display.into(),
        name: format!("Virtual {display}"),
        width: 1920,
        height: 1080,
    }
}

/// A show of `ws` with the stage host `host`, `stage 1` shown on `display` if given.
fn stage_show(ws: Workspace, host: StageHostConfig, display: Option<&str>) -> OfflineShow {
    let mut c: ControlConfig = config(&ws);
    c.stage_host = Some(host);
    if let Some(d) = display {
        c.machine
            .set_stage_mapping(ws.id, stage_id(1), Some(mapping(d)))
            .unwrap();
    }
    let mut s = show_with(ws, c);
    s.settle_stages();
    assert_eq!(s.stalls(), 0, "the stage host did not settle");
    s
}

fn with_stages(cues: Vec<openlab_core::Cue>, stages: Vec<Stage>) -> Workspace {
    let mut ws = workspace(cues);
    ws.stages = stages;
    ws
}

fn request(s: &mut OfflineShow, request: Request) -> Outcome {
    let id = s.request_id();
    let replies = s.send_until(ClientMessage::Request { id, request }, |c| {
        c.stages_settled()
    });
    replies
        .into_iter()
        .find_map(|m| match m {
            ServerMessage::Result { id: got, outcome } if got == id => Some(outcome),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no answer to request {id:?}"))
}

fn ok_data(outcome: Outcome) -> Option<ResultData> {
    match outcome {
        Outcome::Ok(body) => body.data,
        Outcome::Error(e) => panic!("refused: {e:?}"),
    }
}

fn mappings(s: &mut OfflineShow) -> Vec<StageMappingEntry> {
    match ok_data(request(s, Request::ListStageMappings)) {
        Some(ResultData::StageMappings { stages }) => stages,
        other => panic!("{other:?}"),
    }
}

fn displays(s: &mut OfflineShow) -> (Vec<Display>, Option<bool>) {
    match ok_data(request(s, Request::ListDisplays)) {
        Some(ResultData::Displays { displays, video }) => (displays, video),
        other => panic!("{other:?}"),
    }
}

/// What stage `n` shows: the barcode's frame number, and the colours at the centres of the
/// test card's four quadrants.
fn probe(s: &mut OfflineShow, n: u128) -> (Option<u16>, Vec<[u8; 4]>) {
    let points = vec![[0.25, 0.3], [0.75, 0.3], [0.25, 0.8], [0.75, 0.8]];
    match ok_data(request(
        s,
        Request::StageProbe {
            stage: stage_id(n),
            points,
        },
    )) {
        Some(ResultData::StageProbe { frame, colours }) => (frame, colours),
        other => panic!("{other:?}"),
    }
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
const QUADRANTS: [[u8; 4]; 4] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 0, 255],
];

fn state(s: &mut OfflineShow, n: u128) -> Option<StageState> {
    mappings(s)
        .into_iter()
        .find(|e| e.stage == stage_id(n))
        .and_then(|e| e.state)
}

fn logged(s: &mut OfflineShow, text: &str) -> bool {
    s.take_outbound().iter().any(|m| match m {
        ServerMessage::Log { message, .. } => message.contains(text),
        ServerMessage::LogEntry { entry, .. } => entry.message.contains(text),
        _ => false,
    })
}

/// VID-01, VID-12: a workspace whose stage is shown on a chosen display opens with the stage
/// up on that display and black (nothing plays); the test card drawn on it reads back as its
/// frame number and its four colours.
#[test]
fn a_stage_on_its_chosen_display_shows_black_and_then_the_test_card() {
    let mut s = stage_show(
        with_stages(vec![], vec![stage(1)]),
        virtual_host(),
        Some("V1"),
    );
    assert_eq!(
        state(&mut s, 1),
        Some(StageState::Shown {
            display: "V1".into()
        })
    );
    assert_eq!(probe(&mut s, 1), (None, vec![BLACK; 4]));
    let id = s.request_id();
    let replies = s.send(ClientMessage::Request {
        id,
        request: Request::StageTestCard {
            stage: stage_id(1),
            frame: 4321,
        },
    });
    assert!(matches!(
        &replies[..],
        [ServerMessage::Result {
            outcome: Outcome::Ok(_),
            ..
        }]
    ));
    assert_eq!(probe(&mut s, 1), (Some(4321), QUADRANTS.to_vec()));
    assert_eq!(s.stalls(), 0);
}

/// The displays the stage host lists are the operator's choices; choosing one shows the stage
/// there and is kept per machine; an unknown display or stage is refused and changes nothing.
#[test]
fn a_stage_display_is_chosen_from_the_listed_displays() {
    let mut s = stage_show(with_stages(vec![], vec![stage(1)]), virtual_host(), None);
    let (list, video) = displays(&mut s);
    assert_eq!(video, Some(true));
    assert_eq!(
        list.iter()
            .map(|d| (d.id.as_str(), d.width, d.height))
            .collect::<Vec<_>>(),
        [("V1", 1920, 1080), ("V2", 1280, 720)]
    );
    let set = |s: &mut OfflineShow, stage: StageId, display: Option<&str>| {
        request(
            s,
            Request::SetStageMapping {
                stage,
                display: display.map(Into::into),
            },
        )
    };
    match set(&mut s, stage_id(1), Some("nope")) {
        Outcome::Error(e) => assert_eq!(e.code, ErrorCode::NotFound),
        o => panic!("{o:?}"),
    }
    match set(&mut s, stage_id(9), Some("V2")) {
        Outcome::Error(e) => assert_eq!(e.code, ErrorCode::NotFound),
        o => panic!("{o:?}"),
    }
    assert_eq!(state(&mut s, 1), Some(StageState::NoDisplay));
    assert!(matches!(
        set(&mut s, stage_id(1), Some("V2")),
        Outcome::Ok(_)
    ));
    s.settle_stages();
    let entry = mappings(&mut s).remove(0);
    assert_eq!(
        entry.mapping,
        Some(StageMapping {
            display: "V2".into(),
            name: "Virtual V2".into(),
            width: 1280,
            height: 720,
        })
    );
    assert_eq!(
        entry.state,
        Some(StageState::Shown {
            display: "V2".into()
        })
    );
    // Cleared: no display again.
    assert!(matches!(set(&mut s, stage_id(1), None), Outcome::Ok(_)));
    s.settle_stages();
    assert_eq!(state(&mut s, 1), Some(StageState::NoDisplay));
}

/// A stage with no display on this machine, or whose display is not connected, is not an
/// error: the workspace opens, the stage says why it is not shown, and the log warns.
#[test]
fn a_stage_without_its_display_opens_with_a_warning() {
    let ws = with_stages(vec![], vec![stage(1), stage(2)]);
    let mut c: ControlConfig = config(&ws);
    c.stage_host = Some(virtual_host());
    c.machine
        .set_stage_mapping(ws.id, stage_id(2), Some(mapping("GONE")))
        .unwrap();
    c.push_log = true;
    let mut s = show_with(ws, c);
    s.settle_stages();
    assert_eq!(state(&mut s, 1), Some(StageState::NoDisplay));
    assert_eq!(
        state(&mut s, 2),
        Some(StageState::DisplayMissing {
            display: "GONE".into()
        })
    );
    assert!(logged(&mut s, "has no display on this computer"));
}

/// Keep-black off (p. 227): with nothing playing the stage's window is hidden.
#[test]
fn a_stage_with_keep_black_off_is_hidden_while_nothing_plays() {
    let mut st = stage(1);
    st.keep_black = false;
    let mut s = stage_show(with_stages(vec![], vec![st]), virtual_host(), Some("V1"));
    assert_eq!(state(&mut s, 1), Some(StageState::Hidden));
}

/// A tone long enough to outlast every wait below.
fn tone_cue() -> openlab_core::Cue {
    let samples = show::media::tone(RATE as usize * 600, RATE);
    audio(1, &wav(&samples), 0)
}

/// Every block rendered from frame `from` on carries the tone: no gap in the sound.
fn frames_are_sound(s: &OfflineShow, from: usize) -> bool {
    let left = s.channel(0);
    let tail = left.get(from..).unwrap_or_default();
    !tail.is_empty()
        && tail
            .chunks(BLOCK as usize)
            .all(|block| block.iter().any(|x| x.abs() > 0.01))
}

/// A crashed stage host (killed) never stops sound: the cue plays on through the crash, the
/// engine starts a new stage host, and the stage is back on its display, black.
#[test]
fn a_killed_stage_host_is_replaced_and_the_sound_plays_on() {
    let mut s = stage_show(
        with_stages(vec![tone_cue()], vec![stage(1)]),
        virtual_host(),
        Some("V1"),
    );
    s.transport(TransportAction::Go { list: list_id() });
    s.run(BLOCK * 20);
    let first = s.control().stage_host_pid().expect("a running stage host");
    let killed = std::process::Command::new("kill")
        .args(["-9", &first.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let before = s.channel(0).len();
    // Sound goes on while the engine notices and replaces the stage host.
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != first)));
    s.run(BLOCK * 20);
    assert!(frames_are_sound(&s, before), "the cue stopped");
    s.settle_stages();
    assert_eq!(
        state(&mut s, 1),
        Some(StageState::Shown {
            display: "V1".into()
        })
    );
    assert_eq!(probe(&mut s, 1), (None, vec![BLACK; 4]));
    assert_eq!(s.stalls(), 0);
}

fn virtual_display(id: &str, width: u32, height: u32) -> Display {
    Display {
        id: id.into(),
        name: format!("Virtual {id}"),
        width,
        height,
    }
}

/// A stage's display unplugged while a cue plays: no crash, the stage carries a warning, the
/// cue plays on (its sound is heard), and the stage is back on the display when it returns.
#[test]
fn a_stage_display_unplugged_mid_cue_warns_and_comes_back() {
    let ws = with_stages(vec![tone_cue()], vec![stage(1)]);
    let mut c: ControlConfig = config(&ws);
    c.stage_host = Some(virtual_host());
    c.push_log = true;
    c.machine
        .set_stage_mapping(ws.id, stage_id(1), Some(mapping("V1")))
        .unwrap();
    let mut s = show_with(ws, c);
    s.settle_stages();
    s.transport(TransportAction::Go { list: list_id() });
    s.run(BLOCK * 4);
    let playing = s.channel(0).len();
    s.control_mut()
        .set_virtual_displays(vec![virtual_display("V2", 1280, 720)]);
    let missing = StageState::DisplayMissing {
        display: "V1".into(),
    };
    assert!(s.wait_for(|c| c.stage_state(stage_id(1)) == Some(&missing)));
    s.run(BLOCK * 8);
    assert_eq!(
        state(&mut s, 1),
        Some(StageState::DisplayMissing {
            display: "V1".into()
        })
    );
    assert!(logged(&mut s, "its display Virtual V1 is not connected"));
    s.control_mut().set_virtual_displays(vec![
        virtual_display("V1", 1920, 1080),
        virtual_display("V2", 1280, 720),
    ]);
    let shown = StageState::Shown {
        display: "V1".into(),
    };
    assert!(s.wait_for(|c| c.stage_state(stage_id(1)) == Some(&shown)));
    s.run(BLOCK * 8);
    assert_eq!(
        state(&mut s, 1),
        Some(StageState::Shown {
            display: "V1".into()
        })
    );
    assert_eq!(probe(&mut s, 1), (None, vec![BLACK; 4]));
    assert!(frames_are_sound(&s, playing), "the cue stopped");
    assert_eq!(s.stalls(), 0);
}

/// A stage host that stops answering is killed after `answer_within` and replaced.
#[test]
fn a_hung_stage_host_is_killed_and_replaced() {
    let mut host = virtual_host();
    host.ping_every = Duration::from_millis(50);
    host.answer_within = Duration::from_millis(300);
    let mut s = stage_show(with_stages(vec![], vec![stage(1)]), host, Some("V1"));
    let first = s.control().stage_host_pid().unwrap();
    s.control_mut().hang_stage_host();
    assert!(s.wait_for(|c| c.stage_host_pid().is_some_and(|p| p != first)));
    assert!(!alive(first), "the hung stage host was not killed");
    s.settle_stages();
    assert_eq!(
        state(&mut s, 1),
        Some(StageState::Shown {
            display: "V1".into()
        })
    );
}

/// The stage host ends with the engine: on shutdown it is asked to quit, and if the engine
/// vanished its input would close, which ends it too. It is never left running.
#[test]
fn the_stage_host_does_not_outlive_the_engine() {
    let mut s = stage_show(
        with_stages(vec![], vec![stage(1)]),
        virtual_host(),
        Some("V1"),
    );
    let pid = s.control().stage_host_pid().unwrap();
    s.shutdown();
    drop(s);
    assert!(gone_within(pid, 5_000), "stage host {pid} still running");
}

/// A stage host that floods the engine with garbage (here a shell printing forever) is killed
/// as broken, and after too many starts video output pauses; sound is never touched.
#[test]
fn a_stage_host_sending_garbage_is_stopped_and_sound_plays_on() {
    let mut host = host_config(&["-c", "yes 'not a message'"]);
    host.program = PathBuf::from("/bin/sh");
    host.max_restarts = 2;
    host.retry_after = Duration::from_secs(3600);
    let ws = with_stages(vec![tone_cue()], vec![stage(1)]);
    let mut c: ControlConfig = config(&ws);
    c.stage_host = Some(host);
    let mut s = show_with(ws, c);
    s.transport(TransportAction::Go { list: list_id() });
    s.run(BLOCK * 4);
    let started = s.channel(0).len();
    let mut stopped = 0;
    let mut paused = false;
    // Each start is killed for flooding; the third report says video output is off.
    for _ in 0..200 {
        s.run(BLOCK * 4);
        for m in s.take_outbound() {
            if let ServerMessage::Log { message, .. } = m
                && message.contains("video output stopped")
            {
                stopped += 1;
                paused |= message.contains("stages are not shown");
            }
        }
        if paused {
            break;
        }
        s.wait_step();
    }
    assert!(paused, "video output never paused ({stopped} stops)");
    assert!(frames_are_sound(&s, started), "the cue stopped");
    assert!(s.control().stage_host_pid().is_none());
}

/// A stage host that never answers `hello` is killed and does not hold the engine.
#[test]
fn a_silent_stage_host_is_killed_after_the_answer_time() {
    let mut host = host_config(&["-c", "cat > /dev/null"]);
    host.program = PathBuf::from("/bin/sh");
    host.answer_within = Duration::from_millis(200);
    host.max_restarts = 1;
    host.retry_after = Duration::from_secs(3600);
    let ws = with_stages(vec![], vec![stage(1)]);
    let mut c: ControlConfig = config(&ws);
    c.stage_host = Some(host);
    c.push_log = true;
    let mut s = show_with(ws, c);
    let mut seen = false;
    for _ in 0..500 {
        s.tick();
        seen |= logged(&mut s, "did not answer hello");
        if seen {
            break;
        }
        s.wait_step();
    }
    assert!(seen);
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether `pid` is gone within `tries` checks (a hang guard: each check runs `kill -0`, so this
/// bounds the wait without reading a clock).
fn gone_within(pid: u32, tries: usize) -> bool {
    (0..tries).any(|_| !alive(pid))
}
