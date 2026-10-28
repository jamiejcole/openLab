// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC address -> the inputs the UI sends and the replies QLab 5 gives (`docs/osc.md` lists them).
//! Two steps so that nothing about the show is revealed before the peer's passcode is checked:
//! [`parse_address`] looks at the address text only; [`resolve`] looks cues up and builds the
//! inputs and replies.

use std::ops::ControlFlow;

use openlab_core::protocol::{ClientMessage, DocId, RequestId, TransportAction};
use openlab_core::{
    Command, CueId, CueListId, CueProperty, NumberAssignment, OscAccess, OscRight, WorkspaceId,
};
use rosc::{OscMessage, OscType};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::pattern::{CuePattern, PatternError, has_pattern_chars};

/// Longest address accepted.
pub const MAX_ADDRESS_BYTES: usize = 256;
/// Most `/`-separated parts in an address.
pub const MAX_ADDRESS_PARTS: usize = 8;
/// Most cues one message may act on or answer for through a pattern (`/cue/*/...`) or
/// `/cue/active/...`. The rest are left out and counted.
pub const MAX_MATCHES: usize = 1024;

/// What the dispatcher needs to know about the open show. The runtime implements it over the
/// OSC task's copy of the workspace; tests use a stand-in. The methods with a default are
/// answered "nothing there" until a show says otherwise.
pub trait OscShow {
    fn workspace_id(&self) -> WorkspaceId;
    fn workspace_name(&self) -> &str {
        ""
    }
    /// The passcode table: what a peer may do without `/connect` and with each passcode.
    fn access(&self) -> &OscAccess;
    /// The cue list `/go` and `/playhead` act on: the one whose playhead was last moved (QLab:
    /// the list on display).
    fn active_list(&self) -> Option<CueListId>;
    /// The cue with exactly this number, in any list, at any depth.
    fn cue_by_number(&self, number: &str) -> Option<CueId>;
    fn cue_exists(&self, id: CueId) -> bool;
    /// The value `/cue/{n}/{field}` reports; `None` when the cue has no such field.
    fn cue_property(&self, id: CueId, field: CueField) -> Option<Value>;
    /// The cue whose OSC trigger (TRG-03) is exactly this address.
    fn cue_by_osc_trigger(&self, address: &str) -> Option<CueId>;
    /// The cue list that holds `cue` directly, not inside a group: a playhead only rests on such
    /// a cue.
    fn top_level_list(&self, _cue: CueId) -> Option<CueListId> {
        None
    }
    /// The cue the list's playhead is on.
    fn playhead(&self, _list: CueListId) -> Option<CueId> {
        None
    }
    fn list_name(&self, _list: CueListId) -> Option<String> {
        None
    }
    /// Cues that are running (any state but idle, loaded and paused; a broken cue is not), and
    /// with `include_paused` the paused ones too, in workspace order.
    fn running_cues(&self, _include_paused: bool) -> Vec<CueId> {
        Vec::new()
    }
    /// Calls `visit` with every cue list and every cue (lists in order, each followed by its cues
    /// depth first) and its number text (empty for a list and a cue without a number), until it
    /// breaks.
    fn visit_targets(&self, _visit: &mut dyn FnMut(Target, &str) -> ControlFlow<()>) {}
    /// The cue as QLab's cue objects in `/runningCues` and friends.
    fn cue_object(&self, _id: CueId) -> Option<Value> {
        None
    }
    /// Every cue list as such an object, each with its cues under `cues`.
    fn cue_lists_object(&self) -> Value {
        json!([])
    }
}

/// One open workspace as the dispatcher sees it: the engine's doc and its show. A call gets every
/// open doc, in open order (ARCHITECTURE section 7: an un-prefixed message goes to each).
#[derive(Clone, Copy)]
pub struct DocShow<'a> {
    pub doc: DocId,
    pub show: &'a dyn OscShow,
}

/// The doc `/workspace/{id}` names: the one with that id, as replies write it (upper case), else
/// the first, in open order, whose name is exactly that (QLab 5: "{id} may be either the display
/// name of the workspace ... or the unique ID").
pub fn find_doc<'a>(docs: &[DocShow<'a>], id: &str) -> Option<DocShow<'a>> {
    docs.iter()
        .find(|d| upper(d.show.workspace_id()) == id)
        .or_else(|| docs.iter().find(|d| d.show.workspace_name() == id))
        .copied()
}

/// What a pattern can match: a cue list (its number is empty) or a cue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    List(CueListId),
    Cue(CueId),
}

/// The cue properties OSC reads and writes. The `Is...` ones and `UniqueId` are read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueField {
    Name,
    Number,
    Notes,
    PreWait,
    PostWait,
    Armed,
    Flagged,
    UniqueId,
    IsLoaded,
    IsRunning,
    IsPaused,
    IsBroken,
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
            Self::Command(command) => ClientMessage::Command {
                id,
                command,
                coalesce: None,
            },
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
    /// A number with pattern characters: every cue (and cue list) whose number matches.
    Pattern(String),
    /// `/cue/selected/...`: the cue at the active list's playhead (the engine has no separate
    /// selection; QLab's default "lock playhead to selection" makes them the same).
    Selected,
    /// `/cue/playhead/...`: the cue at the active list's playhead.
    Playhead,
    /// `/cue/active/...`: every running cue.
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    HardStop,
    Panic,
    Pause,
    Resume,
    Load,
    Reset,
    Preview,
    /// Reads the field with no argument, writes it with one.
    Field(CueField),
}

impl Verb {
    /// The address part for the verb.
    pub fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::HardStop => "hardStop",
            Self::Panic => "panic",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Load => "load",
            Self::Reset => "reset",
            Self::Preview => "preview",
            Self::Field(field) => field.name(),
        }
    }
}

impl CueField {
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Number => "number",
            Self::Notes => "notes",
            Self::PreWait => "preWait",
            Self::PostWait => "postWait",
            Self::Armed => "armed",
            Self::Flagged => "flagged",
            Self::UniqueId => "uniqueID",
            Self::IsLoaded => "isLoaded",
            Self::IsRunning => "isRunning",
            Self::IsPaused => "isPaused",
            Self::IsBroken => "isBroken",
        }
    }
}

/// The queries about the workspace as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    RunningCues,
    RunningOrPausedCues,
    SelectedCues,
    CueLists,
    /// `/playheadID`; `/playheadId` is the deprecated spelling (still answered, with a warning).
    PlayheadId {
        deprecated: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Connect,
    Updates,
    UdpReplyPort,
    Version,
    Workspaces,
    Go,
    Stop,
    HardStop,
    Panic,
    PauseAll,
    ResumeAll,
    Playhead(PlayheadTarget),
    Select(PlayheadTarget),
    Cue {
        cue: CueRef,
        verb: Verb,
    },
    Query(Query),
    /// Not in the API; may still be a cue's own OSC trigger. Holds the address without any
    /// `/workspace/{id}` prefix.
    Other(String),
    /// Contains pattern characters outside the cue-number part. Not expanded; the address (without
    /// prefix) may still be exactly a cue's OSC trigger, which can therefore contain such
    /// characters.
    Pattern(String),
    /// In the API's shape but not something this build does.
    Unsupported(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The id from a `/workspace/{id}/...` prefix, as written.
    pub workspace: Option<String>,
    /// The address without any prefix.
    pub path: String,
    pub address: Address,
}

/// The canonical text of an id in addresses and replies: upper-case, as QLab writes them.
pub fn upper(id: impl std::fmt::Display) -> String {
    id.to_string().to_uppercase()
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
    let path = format!("/{}", rest.join("/"));
    let address = parse_rest(rest, workspace.is_some(), &path);
    Some(Parsed {
        workspace,
        path,
        address,
    })
}

fn parse_rest(rest: &[&str], prefixed: bool, path: &str) -> Address {
    let other = || Address::Other(path.to_owned());
    if rest.iter().any(|p| has_pattern_chars(p)) {
        // Only the cue-number part of `/cue/{number}/{verb}` is a pattern.
        return match rest {
            ["cue", number, verb] if has_pattern_chars(number) && !has_pattern_chars(verb) => {
                match verb_of(verb) {
                    Some(verb) => Address::Cue {
                        cue: CueRef::Pattern((*number).to_owned()),
                        verb,
                    },
                    None => Address::Pattern(path.to_owned()),
                }
            }
            _ => Address::Pattern(path.to_owned()),
        };
    }
    match rest {
        ["connect"] => Address::Connect,
        // Application-level: not available under `/workspace/{id}`.
        ["updates"] if !prefixed => Address::Updates,
        ["udpReplyPort"] if !prefixed => Address::UdpReplyPort,
        ["version"] if !prefixed => Address::Version,
        ["workspaces"] if !prefixed => Address::Workspaces,
        ["go"] => Address::Go,
        ["stop"] => Address::Stop,
        ["hardStop"] => Address::HardStop,
        ["panic"] => Address::Panic,
        ["pause"] => Address::PauseAll,
        ["resume"] => Address::ResumeAll,
        ["playhead", target] => Address::Playhead(target_of(target)),
        ["select", target] => Address::Select(target_of(target)),
        ["runningCues"] => Address::Query(Query::RunningCues),
        ["runningOrPausedCues"] => Address::Query(Query::RunningOrPausedCues),
        ["selectedCues"] => Address::Query(Query::SelectedCues),
        ["cueLists"] => Address::Query(Query::CueLists),
        ["playheadID"] => Address::Query(Query::PlayheadId { deprecated: false }),
        ["playheadId"] => Address::Query(Query::PlayheadId { deprecated: true }),
        ["cue", number, verb] => match verb_of(verb) {
            Some(verb) => Address::Cue {
                cue: match *number {
                    "selected" => CueRef::Selected,
                    "playhead" => CueRef::Playhead,
                    "active" => CueRef::Active,
                    number => CueRef::Number(number.to_owned()),
                },
                verb,
            },
            None => other(),
        },
        ["cue_id", id, verb] => match (verb_of(verb), cue_uuid(id)) {
            (Some(verb), Some(uuid)) => Address::Cue {
                cue: CueRef::Id(CueId(uuid)),
                verb,
            },
            (Some(_), None) => Address::Unsupported("not a cue id"),
            (None, _) => other(),
        },
        _ => other(),
    }
}

/// A cue id as QLab writes it: the hyphenated form in upper case (a lower-case one is not found).
fn cue_uuid(text: &str) -> Option<Uuid> {
    Uuid::parse_str(text)
        .ok()
        .filter(|uuid| upper(uuid) == text)
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
        "panic" => Verb::Panic,
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
        "uniqueID" => Verb::Field(CueField::UniqueId),
        "isLoaded" => Verb::Field(CueField::IsLoaded),
        "isRunning" => Verb::Field(CueField::IsRunning),
        "isPaused" => Verb::Field(CueField::IsPaused),
        "isBroken" => Verb::Field(CueField::IsBroken),
        _ => return None,
    })
}

/// The right a message needs, judged on its address and argument count alone so that nothing
/// about the show is looked up before the peer is known to be allowed (docs/osc.md, "Rights").
/// `None` for `/connect`, `/version`, `/workspaces` and `/updates`, which every peer may send.
///
/// - View: every read, `/udpReplyPort`, and writing a cue's `notes`.
/// - Edit: writing any other cue property (name, number, pre-wait, post-wait, armed, flagged),
///   and `/select`.
/// - Control: transport (`/go`, `/stop`, `/hardStop`, `/panic`, `/pause`, `/resume`, the cue
///   verbs `start` ... `preview`, `/playhead`) and a cue's own OSC trigger address. An address
///   nothing answers to counts as a trigger, so a peer without Control learns nothing about which
///   triggers exist.
pub fn required_right(address: &Address, args: &[OscType]) -> Option<OscRight> {
    Some(match address {
        // QLab: `/updates` works at any access level; what an update carries is filtered by the
        // View right when it is sent (`Osc::may_receive_updates`).
        Address::Connect | Address::Version | Address::Workspaces | Address::Updates => {
            return None;
        }
        Address::UdpReplyPort | Address::Query(_) | Address::Unsupported(_) => OscRight::View,
        Address::Select(_) => OscRight::Edit,
        Address::Go
        | Address::Stop
        | Address::HardStop
        | Address::Panic
        | Address::PauseAll
        | Address::ResumeAll
        | Address::Playhead(_)
        | Address::Other(_)
        | Address::Pattern(_) => OscRight::Control,
        Address::Cue { verb, .. } => match verb {
            // A property without an argument is a read.
            Verb::Field(_) if args.is_empty() => OscRight::View,
            Verb::Field(CueField::Notes) => OscRight::View,
            Verb::Field(_) => OscRight::Edit,
            _ => OscRight::Control,
        },
    })
}

/// Reply status strings.
pub const STATUS_OK: &str = "ok";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_DENIED: &str = "denied";

/// What a reply says about where it comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A workspace message: `address` is `/workspace/{id}` + the path, and `workspace_id` is set.
    Workspace,
    /// An application message (`/version`, `/workspaces`, `/updates`): the address as sent, no
    /// `workspace_id`.
    App,
    /// A message for another workspace: the address as sent, and nothing else but the status.
    Bare,
}

/// A reply, before it is made into a message.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub status: &'static str,
    /// The value; QLab leaves `data` out when there is none (errors, refusals).
    pub data: Option<Value>,
    pub scope: Scope,
    /// The path (under `/workspace/{id}`) the reply is about when it is not the one asked: a
    /// wildcard or `/cue/selected` answers with the cue's own address.
    pub path: Option<String>,
    /// Further keys of the JSON body (`deprecated`, `warning`).
    pub extra: Vec<(&'static str, Value)>,
}

impl Reply {
    fn new(status: &'static str, data: Option<Value>) -> Self {
        Self {
            status,
            data,
            scope: Scope::Workspace,
            path: None,
            extra: Vec::new(),
        }
    }

    pub fn ok(data: Value) -> Self {
        Self::new(STATUS_OK, Some(data))
    }

    pub fn error() -> Self {
        Self::new(STATUS_ERROR, None)
    }

    pub fn denied() -> Self {
        Self::new(STATUS_DENIED, None)
    }

    pub fn scoped(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }
}

/// What resolving a message produced: inputs for the engine (QLab sends no reply for actions),
/// replies for the peer, or both (a wildcard over cues some of which refuse).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    pub inputs: Vec<OscInput>,
    pub replies: Vec<Reply>,
    /// Nothing is known by this address (answered with an error reply too).
    pub unknown: bool,
    /// The message matched more than [`MAX_MATCHES`] cues, or its patterns used up their budget:
    /// what was found is used, the rest is not.
    pub capped: bool,
    /// The inputs start a cue through its own OSC trigger address. That needs Control whatever
    /// the address looks like (a trigger may be shaped like a read), so the caller checks it.
    pub trigger: bool,
}

impl Resolved {
    fn trigger(cue: CueId) -> Self {
        Self {
            trigger: true,
            ..Self::input(TransportAction::Start { cue })
        }
    }

    fn input(action: TransportAction) -> Self {
        Self {
            inputs: vec![OscInput::Transport(action)],
            ..Self::default()
        }
    }

    fn reply(reply: Reply) -> Self {
        Self {
            replies: vec![reply],
            ..Self::default()
        }
    }

    fn error() -> Self {
        Self::reply(Reply::error())
    }
}

/// What the endpoint serves about itself, for `/version` and `/workspaces`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    pub version: String,
    /// The port the OSC sockets listen on.
    pub port: u16,
    /// The UDP port replies go to.
    pub reply_port: u16,
}

/// Answers the application-level queries, which need no passcode. `/workspaces` lists every
/// open doc, in open order.
pub fn app_reply(docs: &[DocShow<'_>], info: &ServerInfo, address: &Address) -> Option<Reply> {
    let reply = match address {
        Address::Version => Reply::ok(json!(info.version)),
        Address::Workspaces => Reply::ok(Value::Array(
            docs.iter()
                .map(|d| {
                    json!({
                        "version": info.version,
                        "displayName": d.show.workspace_name(),
                        "udpReplyPort": info.reply_port,
                        "uniqueID": upper(d.show.workspace_id()),
                        "port": info.port,
                    })
                })
                .collect(),
        )),
        _ => return None,
    };
    Some(reply.scoped(Scope::App))
}

/// Looks cues up and checks arguments for every address except `/connect`, `/updates`,
/// `/udpReplyPort`, `/version` and `/workspaces`, which the session logic owns.
pub fn resolve(
    show: &dyn OscShow,
    parsed: &Parsed,
    args: &[OscType],
    budget: &mut u64,
) -> Resolved {
    let address = &parsed.address;
    match address {
        Address::Connect
        | Address::Updates
        | Address::UdpReplyPort
        | Address::Version
        | Address::Workspaces
        | Address::Unsupported(_) => Resolved::error(),
        Address::Pattern(path) => trigger_or_unknown(show, path),
        Address::Stop => Resolved::input(TransportAction::StopAll),
        Address::HardStop => Resolved::input(TransportAction::HardStopAll),
        Address::Panic => Resolved::input(TransportAction::Panic),
        Address::PauseAll => Resolved::input(TransportAction::PauseAll),
        Address::ResumeAll => Resolved::input(TransportAction::ResumeAll),
        Address::Go => go(show, args),
        Address::Playhead(target) | Address::Select(target) => playhead(show, target),
        Address::Query(query) => query_reply(show, *query),
        Address::Cue { cue, verb } => cue_message(show, parsed, cue, *verb, args, budget),
        Address::Other(path) => trigger_or_unknown(show, path),
    }
}

fn trigger_or_unknown(show: &dyn OscShow, path: &str) -> Resolved {
    match show.cue_by_osc_trigger(path) {
        Some(cue) => Resolved::trigger(cue),
        None => Resolved {
            replies: vec![Reply::error()],
            unknown: true,
            ..Resolved::default()
        },
    }
}

/// `/go`: the active list's playhead cue; with a string argument, from the cue with that number
/// (QLab: the playhead moves there first; one `go_from` input). Other argument types are ignored.
fn go(show: &dyn OscShow, args: &[OscType]) -> Resolved {
    let Some(OscType::String(number)) = args.first() else {
        return match show.active_list() {
            Some(list) => Resolved::input(TransportAction::Go { list }),
            None => Resolved::error(),
        };
    };
    // Nothing is started unless the playhead can go to the cue: a GO from the wrong cue in a
    // live show is worse than none.
    let found = show
        .cue_by_number(number)
        .and_then(|cue| show.top_level_list(cue).map(|list| (cue, list)));
    let Some((cue, list)) = found else {
        return Resolved::error();
    };
    // One input: if the copy is stale and the cue is gone, the engine starts nothing.
    Resolved::input(TransportAction::GoFrom { list, cue })
}

fn playhead(show: &dyn OscShow, target: &PlayheadTarget) -> Resolved {
    let action = match target {
        PlayheadTarget::Next | PlayheadTarget::Previous => {
            let Some(list) = show.active_list() else {
                return Resolved::error();
            };
            if *target == PlayheadTarget::Next {
                TransportAction::PlayheadNext { list }
            } else {
                TransportAction::PlayheadPrevious { list }
            }
        }
        PlayheadTarget::Number(number) => {
            // Any list: a cue in another list takes that list's playhead (QLab switches the
            // displayed list to it).
            let list = show
                .cue_by_number(number)
                .and_then(|cue| show.top_level_list(cue));
            let Some(list) = list else {
                return Resolved::error();
            };
            TransportAction::SetPlayheadByNumber {
                list,
                number: number.clone(),
            }
        }
    };
    Resolved::input(action)
}

fn query_reply(show: &dyn OscShow, query: Query) -> Resolved {
    let objects = |ids: Vec<CueId>| {
        Reply::ok(Value::Array(
            ids.into_iter()
                .filter_map(|id| show.cue_object(id))
                .collect(),
        ))
    };
    let playhead = || {
        show.active_list()
            .and_then(|list| show.playhead(list))
            .filter(|cue| show.cue_exists(*cue))
    };
    Resolved::reply(match query {
        Query::RunningCues => objects(show.running_cues(false)),
        Query::RunningOrPausedCues => objects(show.running_cues(true)),
        Query::SelectedCues => objects(playhead().into_iter().collect()),
        Query::CueLists => Reply::ok(show.cue_lists_object()),
        Query::PlayheadId { deprecated } => {
            let id = playhead().map_or_else(|| "none".to_owned(), upper);
            let mut reply = Reply::ok(json!(id));
            if deprecated {
                reply.path = Some("/playheadID".to_owned());
                reply.extra = vec![
                    ("deprecated", json!(true)),
                    (
                        "warning",
                        json!("/playheadId is deprecated; use /playheadID (capital D)"),
                    ),
                ];
            }
            reply
        }
    })
}

/// `/cue/...` and `/cue_id/...`: one cue, a pattern's cues, or the playhead / running cues.
fn cue_message(
    show: &dyn OscShow,
    parsed: &Parsed,
    cue: &CueRef,
    verb: Verb,
    args: &[OscType],
    budget: &mut u64,
) -> Resolved {
    let mut out = Resolved::default();
    // Pseudo-cues answer for a property; an action on "the selected cue" is not part of the API.
    let property = matches!(verb, Verb::Field(_));
    let targets: Vec<Target> = match cue {
        CueRef::Id(id) => show
            .cue_exists(*id)
            .then_some(Target::Cue(*id))
            .into_iter()
            .collect(),
        CueRef::Number(number) => show
            .cue_by_number(number)
            .map(Target::Cue)
            .into_iter()
            .collect(),
        CueRef::Selected | CueRef::Playhead if property => show
            .active_list()
            .and_then(|list| show.playhead(list))
            .filter(|cue| show.cue_exists(*cue))
            .map(Target::Cue)
            .into_iter()
            .collect(),
        CueRef::Active if property => {
            let running = show.running_cues(false);
            out.capped = running.len() > MAX_MATCHES;
            running
                .into_iter()
                .take(MAX_MATCHES)
                .map(Target::Cue)
                .collect()
        }
        CueRef::Selected | CueRef::Playhead | CueRef::Active => Vec::new(),
        CueRef::Pattern(pattern) => match CuePattern::compile_within(pattern, budget) {
            Ok(pattern) => matching(show, &pattern, budget, &mut out),
            Err(PatternError::Budget) => {
                out.capped = true;
                Vec::new()
            }
            Err(_) => Vec::new(),
        },
    };
    if targets.is_empty() {
        // QLab: an error for an address that reaches nothing (the address as asked).
        if let Some(cue) = pattern_trigger(show, parsed, cue) {
            return Resolved::trigger(cue);
        }
        out.replies.push(Reply::error());
        return out;
    }
    // Replies about a cue found by pattern or as "the playhead" name that cue, as QLab's do.
    let rewrite = !matches!(cue, CueRef::Number(_) | CueRef::Id(_));
    for target in targets {
        let before = out.replies.len();
        match target {
            Target::Cue(id) => cue_verb(show, id, verb, args, &mut out),
            Target::List(list) => list_verb(show, list, verb, args, &mut out),
        }
        if rewrite {
            let path = target_path(show, target, verb);
            for reply in out.replies.iter_mut().skip(before) {
                if reply.path.is_none() {
                    reply.path = Some(path.clone());
                }
            }
        }
    }
    out
}

/// A cue whose own OSC trigger is this very address (a trigger may contain pattern characters,
/// and it is looked up first for them).
fn pattern_trigger(show: &dyn OscShow, parsed: &Parsed, cue: &CueRef) -> Option<CueId> {
    matches!(cue, CueRef::Pattern(_))
        .then(|| show.cue_by_osc_trigger(&parsed.path))
        .flatten()
}

/// The targets whose number matches, in workspace order, at most [`MAX_MATCHES`].
fn matching(
    show: &dyn OscShow,
    pattern: &CuePattern,
    budget: &mut u64,
    out: &mut Resolved,
) -> Vec<Target> {
    let mut found = Vec::new();
    show.visit_targets(
        &mut |target, number| match pattern.is_match(number, budget) {
            Ok(true) if found.len() >= MAX_MATCHES => {
                out.capped = true;
                ControlFlow::Break(())
            }
            Ok(true) => {
                found.push(target);
                ControlFlow::Continue(())
            }
            Ok(false) => ControlFlow::Continue(()),
            Err(_) => {
                out.capped = true;
                ControlFlow::Break(())
            }
        },
    );
    found
}

/// The path QLab puts in the reply about a cue found indirectly: its number, or its id when it
/// has none (a cue list never has one).
fn target_path(show: &dyn OscShow, target: Target, verb: Verb) -> String {
    let verb = verb.name();
    match target {
        Target::List(list) => format!("/cue_id/{}/{verb}", upper(list.0)),
        Target::Cue(id) => match show.cue_property(id, CueField::Number) {
            Some(Value::String(number)) if !number.is_empty() => format!("/cue/{number}/{verb}"),
            _ => format!("/cue_id/{}/{verb}", upper(id)),
        },
    }
}

/// A cue list answers for its name and number only; any other verb skips it, so `/cue/*/start`
/// does not start lists and `/cue/*/armed` is not full of errors.
fn list_verb(
    show: &dyn OscShow,
    list: CueListId,
    verb: Verb,
    args: &[OscType],
    out: &mut Resolved,
) {
    if !args.is_empty() {
        return;
    }
    match verb {
        Verb::Field(CueField::Name) => {
            if let Some(name) = show.list_name(list) {
                out.replies.push(Reply::ok(json!(name)));
            }
        }
        Verb::Field(CueField::Number) => out.replies.push(Reply::ok(json!(""))),
        Verb::Field(CueField::UniqueId) => out.replies.push(Reply::ok(json!(upper(list.0)))),
        _ => {}
    }
}

fn cue_verb(show: &dyn OscShow, cue: CueId, verb: Verb, args: &[OscType], out: &mut Resolved) {
    let transport = |out: &mut Resolved, action| {
        out.inputs.push(OscInput::Transport(action));
    };
    let bad = |out: &mut Resolved| out.replies.push(Reply::error());
    let has_args = !args.is_empty();
    match verb {
        Verb::Start
        | Verb::Stop
        | Verb::HardStop
        | Verb::Panic
        | Verb::Pause
        | Verb::Resume
        | Verb::Reset
        | Verb::Preview
            if has_args =>
        {
            bad(out);
        }
        Verb::Start => transport(out, TransportAction::Start { cue }),
        Verb::Stop => transport(
            out,
            TransportAction::Stop {
                cue,
                fade_out: None,
            },
        ),
        Verb::HardStop => transport(out, TransportAction::HardStop { cue }),
        Verb::Panic => transport(out, TransportAction::PanicCue { cue }),
        Verb::Pause => transport(out, TransportAction::Pause { cue }),
        Verb::Resume => transport(out, TransportAction::Resume { cue }),
        Verb::Reset => transport(out, TransportAction::Reset { cue }),
        Verb::Preview => transport(out, TransportAction::Preview { cue }),
        Verb::Load => match args {
            [] => transport(out, TransportAction::Load { cue, at: 0.0 }),
            [one] => match number_arg(one).filter(|t| *t >= 0.0) {
                Some(at) => transport(out, TransportAction::Load { cue, at }),
                None => bad(out),
            },
            _ => bad(out),
        },
        Verb::Field(field) => {
            if args.is_empty() {
                match show.cue_property(cue, field) {
                    Some(value) => out.replies.push(Reply::ok(value)),
                    None => bad(out),
                }
                return;
            }
            match set_field(cue, field, args) {
                Some(command) => out.inputs.push(OscInput::Command(Box::new(command))),
                None => bad(out),
            }
        }
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
        // Read-only.
        CueField::UniqueId
        | CueField::IsLoaded
        | CueField::IsRunning
        | CueField::IsPaused
        | CueField::IsBroken => return None,
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

/// The argument of `/connect`: `None` when there is none (the peer connects without a passcode),
/// else the string, or an integer as text (QLab takes `1234` sent as an int32 like the string).
/// Anything else (another type, several arguments) is `Some("")`: no passcode in the table is
/// empty, so it is a wrong passcode where the table has any, and never a connect without one.
pub fn passcode_arg(args: &[OscType]) -> Option<String> {
    match args {
        [] => None,
        [OscType::String(s)] => Some(s.clone()),
        // Numbers never match: a passcode with a leading zero cannot be told from a shorter one,
        // and QLab sends the passcode as a string.
        _ => Some(String::new()),
    }
}

/// The argument of `/updates`: 0/1 or a boolean; `None` for anything else.
pub fn updates_arg(args: &[OscType]) -> Option<bool> {
    match args {
        [arg] => flag_arg(arg),
        _ => None,
    }
}

/// The argument of `/udpReplyPort`: one int in 1-65535; `None` for anything else.
pub fn reply_port_arg(args: &[OscType]) -> Option<u16> {
    match args {
        [OscType::Int(port)] => u16::try_from(*port).ok().filter(|p| *p != 0),
        _ => None,
    }
}

/// Longest JSON body of a reply to a peer that has not proved it may ask (a passcode is set and
/// it has not sent `/connect`). Such a peer only ever gets small refusals, and may be a forged
/// source address: the cap keeps what we send at it near one unfragmented datagram (1400 bytes
/// fits a 1500-byte MTU with headers).
pub const MAX_REPLY_JSON_BYTES: usize = 1400;
/// The most a UDP datagram can carry (65535 minus IP and UDP headers). A reply to a peer that
/// may ask is allowed up to this over UDP, as QLab sends 60 KB replies.
pub const MAX_UDP_DATAGRAM_BYTES: usize = 65_507;

/// A QLab-style reply message: address `/reply` + the address that was asked, one string
/// argument holding the JSON body (`status` and `address` always, `workspace_id` for workspace
/// messages, `data` when there is a value). `path` is the asked address without any workspace
/// prefix. A body over `max_json` bytes is replaced by a plain error reply, which is smaller.
pub fn reply_message(
    workspace: WorkspaceId,
    asked: &str,
    path: &str,
    reply: &Reply,
    max_json: usize,
) -> OscMessage {
    let build = |status: &str, data: Option<&Value>, scope: Scope, extra: &[(&str, Value)]| {
        let mut body = Map::new();
        body.insert("status".into(), json!(status));
        body.insert(
            "address".into(),
            json!(match scope {
                Scope::Workspace => format!(
                    "/workspace/{}{}",
                    upper(workspace),
                    reply.path.as_deref().unwrap_or(path)
                ),
                Scope::App | Scope::Bare => asked.to_owned(),
            }),
        );
        if scope == Scope::Workspace {
            body.insert("workspace_id".into(), json!(upper(workspace)));
        }
        if let Some(data) = data {
            body.insert("data".into(), data.clone());
        }
        for (key, value) in extra {
            body.insert((*key).to_owned(), value.clone());
        }
        Value::Object(body).to_string()
    };
    let addr = format!("/reply{asked}");
    let mut body = build(reply.status, reply.data.as_ref(), reply.scope, &reply.extra);
    if body.len() > max_json {
        body = build(STATUS_ERROR, None, reply.scope, &[]);
    }
    OscMessage {
        addr,
        args: vec![OscType::String(body)],
    }
}
