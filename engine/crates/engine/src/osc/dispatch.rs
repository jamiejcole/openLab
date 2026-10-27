// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC address -> the inputs the UI sends (`docs/osc.md` lists them). Two steps so that nothing
//! about the show is revealed before the peer's passcode is checked: [`parse_address`] looks at
//! the address text only; [`resolve`] looks cues up and builds the input or the reply.

use openlab_core::protocol::{ClientMessage, RequestId, TransportAction};
use openlab_core::{Command, CueId, CueListId, CueProperty, NumberAssignment, WorkspaceId};
use rosc::{OscMessage, OscType};
use serde_json::{Value, json};
use uuid::Uuid;

/// Longest address accepted.
pub const MAX_ADDRESS_BYTES: usize = 256;
/// Most `/`-separated parts in an address.
pub const MAX_ADDRESS_PARTS: usize = 8;

/// What the dispatcher needs to know about the open show. The runtime implements it over the
/// control thread's view of the workspace; tests use a stand-in.
pub trait OscShow {
    fn workspace_id(&self) -> WorkspaceId;
    /// The workspace passcode. `None` or empty: no passcode, no `/connect` needed.
    fn passcode(&self) -> Option<&str>;
    /// The cue list `/go` and `/playhead` act on.
    fn active_list(&self) -> Option<CueListId>;
    fn cue_by_number(&self, number: &str) -> Option<CueId>;
    fn cue_exists(&self, id: CueId) -> bool;
    /// The value `/cue/{n}/{field}` reports; `None` when the cue has no such field.
    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value>;
    /// The cue whose OSC trigger (TRG-03) is exactly this address.
    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId>;
}

/// The cue properties OSC reads and writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueField {
    Name,
    Number,
    Notes,
    PreWait,
    PostWait,
    Armed,
    Flagged,
}

/// What an accepted message asks the engine to do: the same inputs the UI sends.
#[derive(Clone, Debug, PartialEq)]
pub enum OscInput {
    Transport(TransportAction),
    Command(Box<Command>),
}

impl OscInput {
    /// The protocol message for it; the runtime picks the request id.
    pub fn into_client_message(self, id: RequestId) -> ClientMessage {
        match self {
            Self::Transport(action) => ClientMessage::Transport { id, action },
            Self::Command(command) => ClientMessage::Command { id, command },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayheadTarget {
    Next,
    Previous,
    Number(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CueRef {
    Number(String),
    Id(CueId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    HardStop,
    Pause,
    Resume,
    Load,
    Reset,
    Preview,
    /// Reads the field with no argument, writes it with one.
    Field(CueField),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Connect,
    Updates,
    Go,
    Stop,
    Panic,
    PauseAll,
    ResumeAll,
    Playhead(PlayheadTarget),
    Select(PlayheadTarget),
    Cue {
        cue: CueRef,
        verb: Verb,
    },
    /// Not in the API; may still be a cue's own OSC trigger. Holds the address without any
    /// `/workspace/{id}` prefix.
    Other(String),
    /// In the API's shape but not something this build does; the text says why.
    Unsupported(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The id from a `/workspace/{id}/...` prefix.
    pub workspace: Option<String>,
    pub address: Address,
}

/// Reads the address text. `None` if it is not a usable address at all (too long, empty parts).
pub fn parse_address(address: &str) -> Option<Parsed> {
    if address.len() > MAX_ADDRESS_BYTES {
        return None;
    }
    let path = address.strip_prefix('/')?;
    // One trailing slash is tolerated; "//" anywhere else is not.
    let path = path.strip_suffix('/').unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > MAX_ADDRESS_PARTS || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let (workspace, rest) = match parts.as_slice() {
        ["workspace", id, rest @ ..] if !rest.is_empty() => (Some((*id).to_owned()), rest),
        _ => (None, parts.as_slice()),
    };
    // Wildcards and alternatives in an address pattern are not expanded.
    if rest
        .iter()
        .any(|p| p.contains(['*', '?', '[', ']', '{', '}']))
    {
        return Some(Parsed {
            workspace,
            address: Address::Unsupported("address patterns are not supported"),
        });
    }
    let other = || Address::Other(format!("/{}", rest.join("/")));
    let address = match rest {
        ["connect"] => Address::Connect,
        ["updates"] => Address::Updates,
        ["go"] => Address::Go,
        ["stop"] => Address::Stop,
        ["panic"] => Address::Panic,
        ["pause"] => Address::PauseAll,
        ["resume"] => Address::ResumeAll,
        ["playhead", target] => Address::Playhead(target_of(target)),
        ["select", target] => Address::Select(target_of(target)),
        ["cue", number, verb] => match verb_of(verb) {
            Some(verb) => Address::Cue {
                cue: CueRef::Number((*number).to_owned()),
                verb,
            },
            None => other(),
        },
        ["cue_id", id, verb] => match (verb_of(verb), Uuid::parse_str(id)) {
            (Some(verb), Ok(uuid)) => Address::Cue {
                cue: CueRef::Id(CueId(uuid)),
                verb,
            },
            (Some(_), Err(_)) => Address::Unsupported("not a cue id"),
            (None, _) => other(),
        },
        _ => other(),
    };
    Some(Parsed { workspace, address })
}

fn target_of(part: &str) -> PlayheadTarget {
    match part {
        "next" => PlayheadTarget::Next,
        "previous" => PlayheadTarget::Previous,
        number => PlayheadTarget::Number(number.to_owned()),
    }
}

fn verb_of(part: &str) -> Option<Verb> {
    Some(match part {
        "start" => Verb::Start,
        "stop" => Verb::Stop,
        "hardStop" => Verb::HardStop,
        "pause" => Verb::Pause,
        "resume" => Verb::Resume,
        "load" => Verb::Load,
        "reset" => Verb::Reset,
        "preview" => Verb::Preview,
        "name" => Verb::Field(CueField::Name),
        "number" => Verb::Field(CueField::Number),
        "notes" => Verb::Field(CueField::Notes),
        "preWait" => Verb::Field(CueField::PreWait),
        "postWait" => Verb::Field(CueField::PostWait),
        "armed" => Verb::Field(CueField::Armed),
        "flagged" => Verb::Field(CueField::Flagged),
        _ => return None,
    })
}

/// Reply status strings.
pub const STATUS_OK: &str = "ok";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_DENIED: &str = "denied";

/// A reply, before it is made into a message.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub status: &'static str,
    pub data: Value,
}

impl Reply {
    pub fn ok(data: Value) -> Self {
        Self {
            status: STATUS_OK,
            data,
        }
    }

    pub fn error(why: &str) -> Self {
        Self {
            status: STATUS_ERROR,
            data: json!(why),
        }
    }

    pub fn denied() -> Self {
        Self {
            status: STATUS_DENIED,
            data: json!(""),
        }
    }
}

/// The outcome of resolving a message against the show.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// Do this; QLab sends no reply for actions.
    Input(OscInput),
    Reply(Reply),
    /// Nothing known by this address.
    Unknown,
}

/// Looks cues up and checks arguments for every address except `/connect` and `/updates`, which
/// the session logic owns.
pub fn resolve(show: &dyn OscShow, address: &Address, args: &[OscType]) -> Resolved {
    use Resolved::{Input, Reply as Rep};
    let transport = |action| Input(OscInput::Transport(action));
    match address {
        Address::Connect | Address::Updates => Resolved::Unknown,
        Address::Unsupported(why) => Rep(Reply::error(why)),
        Address::Stop => Rep(Reply::error("stop all is not supported; use /panic")),
        Address::Panic => transport(TransportAction::Panic),
        Address::PauseAll => transport(TransportAction::PauseAll),
        Address::ResumeAll => transport(TransportAction::ResumeAll),
        Address::Go => match show.active_list() {
            Some(list) => transport(TransportAction::Go { list }),
            None => Rep(Reply::error("no cue list")),
        },
        Address::Playhead(target) | Address::Select(target) => {
            let Some(list) = show.active_list() else {
                return Rep(Reply::error("no cue list"));
            };
            transport(match target {
                PlayheadTarget::Next => TransportAction::PlayheadNext { list },
                PlayheadTarget::Previous => TransportAction::PlayheadPrevious { list },
                PlayheadTarget::Number(number) => TransportAction::SetPlayheadByNumber {
                    list,
                    number: number.clone(),
                },
            })
        }
        Address::Cue { cue, verb } => {
            let id = match cue {
                CueRef::Id(id) => show.cue_exists(*id).then_some(*id),
                CueRef::Number(number) => show.cue_by_number(number),
            };
            match id {
                Some(id) => cue_verb(show, id, *verb, args),
                None => Rep(Reply::error("no such cue")),
            }
        }
        Address::Other(path) => match show.cue_by_osc_trigger(path) {
            Some(cue) => transport(TransportAction::Start { cue }),
            None => Resolved::Unknown,
        },
    }
}

fn cue_verb(show: &dyn OscShow, cue: CueId, verb: Verb, args: &[OscType]) -> Resolved {
    use Resolved::{Input, Reply as Rep};
    let transport = |action| Input(OscInput::Transport(action));
    let no_args = || {
        args.is_empty()
            .then_some(())
            .ok_or_else(|| Reply::error("unexpected argument"))
    };
    match verb {
        Verb::Start => with(no_args(), || transport(TransportAction::Start { cue })),
        Verb::Stop => with(no_args(), || {
            transport(TransportAction::Stop {
                cue,
                fade_out: None,
            })
        }),
        Verb::HardStop => with(no_args(), || transport(TransportAction::HardStop { cue })),
        Verb::Pause => with(no_args(), || transport(TransportAction::Pause { cue })),
        Verb::Resume => with(no_args(), || transport(TransportAction::Resume { cue })),
        Verb::Reset => with(no_args(), || transport(TransportAction::Reset { cue })),
        Verb::Preview => with(no_args(), || transport(TransportAction::Preview { cue })),
        Verb::Load => match args {
            [] => transport(TransportAction::Load { cue, at: 0.0 }),
            [one] => match number_arg(one).filter(|t| *t >= 0.0) {
                Some(at) => transport(TransportAction::Load { cue, at }),
                None => Rep(Reply::error("bad argument")),
            },
            _ => Rep(Reply::error("unexpected argument")),
        },
        Verb::Field(field) => {
            if args.is_empty() {
                return match show.cue_property(cue, field) {
                    Some(value) => Rep(Reply::ok(value)),
                    None => Rep(Reply::error("cue has no such property")),
                };
            }
            match set_field(cue, field, args) {
                Some(command) => Input(OscInput::Command(Box::new(command))),
                None => Rep(Reply::error("bad argument")),
            }
        }
    }
}

fn with(check: Result<(), Reply>, ok: impl FnOnce() -> Resolved) -> Resolved {
    match check {
        Ok(()) => ok(),
        Err(reply) => Resolved::Reply(reply),
    }
}

fn set_field(cue: CueId, field: CueField, args: &[OscType]) -> Option<Command> {
    let [arg] = args else { return None };
    let set = |property| Command::SetCueProperty { id: cue, property };
    Some(match field {
        CueField::Name => set(CueProperty::Name {
            value: string_arg(arg)?,
        }),
        CueField::Notes => set(CueProperty::Notes {
            value: string_arg(arg)?,
        }),
        CueField::Number => {
            let text = match arg {
                OscType::String(s) => s.clone(),
                OscType::Int(i) => i.to_string(),
                OscType::Long(i) => i.to_string(),
                OscType::Float(_) | OscType::Double(_) => format_number(number_arg(arg)?),
                _ => return None,
            };
            Command::SetCueNumbers {
                numbers: vec![NumberAssignment {
                    id: cue,
                    // An empty string clears the number.
                    number: (!text.is_empty()).then_some(text),
                }],
            }
        }
        CueField::PreWait => set(CueProperty::PreWait {
            value: number_arg(arg).filter(|v| *v >= 0.0)?,
        }),
        CueField::PostWait => set(CueProperty::PostWait {
            value: number_arg(arg).filter(|v| *v >= 0.0)?,
        }),
        CueField::Armed => set(CueProperty::Armed {
            value: flag_arg(arg)?,
        }),
        CueField::Flagged => set(CueProperty::Flagged {
            value: flag_arg(arg)?,
        }),
    })
}

fn string_arg(arg: &OscType) -> Option<String> {
    match arg {
        OscType::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// A finite number from any numeric argument.
fn number_arg(arg: &OscType) -> Option<f64> {
    let value = match arg {
        OscType::Int(i) => f64::from(*i),
        OscType::Float(f) => f64::from(*f),
        OscType::Double(d) => *d,
        // Long values beyond 2^53 lose precision; no property here needs them.
        OscType::Long(l) => *l as f64,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

/// 0/1 as an integer, or an OSC boolean.
fn flag_arg(arg: &OscType) -> Option<bool> {
    match arg {
        OscType::Bool(b) => Some(*b),
        OscType::Int(0) => Some(false),
        OscType::Int(1) => Some(true),
        _ => None,
    }
}

/// A number sent as a float, as cue-number text: `2.0` is "2", `1.5` is "1.5".
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// The argument of `/connect`: a string; anything else counts as no passcode given.
pub fn passcode_arg(args: &[OscType]) -> Option<&str> {
    match args {
        [OscType::String(s)] => Some(s),
        _ => None,
    }
}

/// The argument of `/updates`: 0/1 or a boolean; `None` for anything else.
pub fn updates_arg(args: &[OscType]) -> Option<bool> {
    match args {
        [arg] => flag_arg(arg),
        _ => None,
    }
}

/// A QLab-style reply message: address `/reply` + the address that was asked, one string
/// argument holding `{"workspace_id", "address", "status", "data"}` as JSON.
pub fn reply_message(workspace: WorkspaceId, asked: &str, reply: &Reply) -> OscMessage {
    let body = json!({
        "workspace_id": workspace.to_string(),
        "address": asked,
        "status": reply.status,
        "data": reply.data,
    });
    OscMessage {
        addr: format!("/reply{asked}"),
        args: vec![OscType::String(body.to_string())],
    }
}
