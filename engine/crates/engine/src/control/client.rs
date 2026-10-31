// SPDX-License-Identifier: GPL-3.0-or-later

//! Client messages: which doc (or the app) each is for, the app-scope ones, and a doc's
//! transport, command, undo/redo and request handling.

use std::path::PathBuf;

use openlab_core::Command;
use openlab_core::protocol::{
    ClientMessage, ErrorCode, HotkeyPhase, MAX_COALESCE_KEY, OkBody, Outcome, PatchMappingEntry,
    Request, ResultData, ServerMessage, TransportAction,
};

use super::docs::DocSource;
use super::inbox::reply;
use super::watch::WatchAnswer;
use super::{Control, DocCx, DocId, Flow, Inbox, ReplyTx, fail, failed, ok, refused};

impl Control {
    /// Handles one inbox message. A connection's message goes to the doc it is bound to, or is
    /// one of the app-scope messages ([`Self::app_scope`]); `Inbox::Client` (tests, the offline
    /// driver) addresses the first open doc. A worker's answer goes to the doc it was for, and one for
    /// a doc closed since is dropped.
    pub fn handle(&mut self, msg: Inbox) -> Flow {
        match msg {
            Inbox::Client { message, reply: to } => {
                let first = self.docs.first().map(|d| d.id);
                return self.client(Addr::Doc(first), message, &to);
            }
            Inbox::Conn {
                doc: Some(doc),
                message,
                reply: to,
            } => return self.client(Addr::Doc(Some(doc)), message, &to),
            Inbox::Conn {
                doc: None,
                message,
                reply: to,
            } => return self.client(Addr::App, message, &to),
            Inbox::Probe(doc, answer) => {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.probe_answer(answer);
                }
            }
            Inbox::Device(answer) => self.device_answer(answer),
            Inbox::Net(doc, answer) => {
                if let Some(mut cx) = self.cx_of(doc) {
                    cx.net_answer(answer);
                }
            }
            Inbox::Waveform {
                doc,
                ticket,
                result,
            } => {
                self.eng.waveform_reader.finished(ticket);
                if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc) {
                    d.waveforms.answer(ticket, result);
                }
            }
            Inbox::Doc(answer) => self.doc_answer(answer),
            Inbox::Watch(WatchAnswer::Batch(batch)) => self.eng.watch_batches.push(batch),
            Inbox::Watch(WatchAnswer::BucketAnswered { bucket, healthy }) => {
                self.eng.watch.watch_mut().answered(&bucket, healthy);
            }
            Inbox::Log { level, message } => self.eng.log(level, message),
            Inbox::Trigger { source, detail } => self.eng.observe_trigger(source, detail),
            Inbox::Shutdown => return Flow::Shutdown,
        }
        Flow::Continue
    }

    /// A client message for doc `doc` (what a connection bound to it sends, M4.15f). An unknown
    /// or closed doc answers `no_workspace`.
    pub fn handle_doc(&mut self, doc: DocId, message: ClientMessage, to: &ReplyTx) -> Flow {
        self.client(Addr::Doc(Some(doc)), message, to)
    }

    /// A client message: app-scope ones first, whoever sends them; the rest act on the doc the
    /// sender is bound to.
    fn client(&mut self, from: Addr, message: ClientMessage, to: &ReplyTx) -> Flow {
        let message = match self.app_scope(from, message, to) {
            Ok(flow) => return flow,
            Err(message) => message,
        };
        match from {
            Addr::Doc(Some(doc)) => match message {
                // Saves look at the other docs' show files: the engine's, not the doc's.
                ClientMessage::Request {
                    id,
                    request: request @ (Request::Save | Request::SaveAs { .. }),
                } if self.cx_of(doc).is_some() => self.save(doc, id, request, to),
                // The folder walk is the document worker's, not the doc's.
                ClientMessage::Request {
                    id,
                    request: Request::Relink { folder },
                } if self.cx_of(doc).is_some() => {
                    self.relink(doc, id, folder, to);
                    Flow::Continue
                }
                // The mapping is the machine's: every doc resolves its patches again.
                ClientMessage::Request {
                    id,
                    request: Request::SetPatchMapping { patch, mapping },
                } if self.cx_of(doc).is_some() => {
                    self.set_patch_mapping(doc, id, patch, mapping, to)
                }
                message => match self.cx_of(doc) {
                    Some(mut cx) => cx.client(message, to),
                    None => no_doc(message, to),
                },
            },
            Addr::Doc(None) => no_doc(message, to),
            Addr::App => match message {
                // The one transport an app connection may send: Esc, which is app-scope.
                ClientMessage::Transport {
                    id,
                    action: TransportAction::Panic,
                } => {
                    self.panic_all();
                    reply(to, ok(id, None));
                    Flow::Continue
                }
                // The app connection's "snapshot" is the workspace list.
                ClientMessage::Request {
                    id,
                    request: Request::Snapshot,
                } => {
                    reply(
                        to,
                        ServerMessage::Workspaces {
                            workspaces: self.workspace_list(),
                        },
                    );
                    reply(to, ok(id, None));
                    Flow::Continue
                }
                message => no_doc(message, to),
            },
        }
    }

    /// Carries out an app-scope message (ARCHITECTURE §6: on any connection, whether or not a
    /// doc is open): opening a workspace, the recovery scan, closing one by id, the Esc panic
    /// and hotkeys of every doc, the PB-10 override, the device list and the show log. Any
    /// other message is handed back.
    fn app_scope(
        &mut self,
        from: Addr,
        message: ClientMessage,
        to: &ReplyTx,
    ) -> Result<Flow, ClientMessage> {
        match message {
            ClientMessage::Request { id, request } => match request {
                Request::SetOutputOverrides { network_disabled } => {
                    self.set_network_disabled(network_disabled);
                    reply(to, ok(id, None));
                }
                Request::New {} => self.open_doc(DocSource::New, id, to),
                Request::Open { path, recovery } => self.open_doc(
                    DocSource::Open {
                        path: PathBuf::from(path),
                        recovery,
                    },
                    id,
                    to,
                ),
                Request::OpenRecovery {
                    workspace_id,
                    path_hash,
                    modified_ms,
                } => self.open_doc(
                    DocSource::Recovery {
                        workspace_id,
                        path_hash,
                        modified_ms,
                    },
                    id,
                    to,
                ),
                Request::ScanRecovery => self.scan_recovery(id, to),
                Request::ListDevices => reply(
                    to,
                    ServerMessage::Result {
                        id,
                        outcome: Outcome::Ok(OkBody {
                            revision: None,
                            data: Some(ResultData::Devices {
                                devices: self.eng.device_list(),
                                selected: self.eng.config.machine.output_device.clone(),
                            }),
                        }),
                    },
                ),
                Request::SetOutputDevice { device } => {
                    let outcome = self.eng.set_output_device(device);
                    reply(
                        to,
                        match outcome {
                            Ok(()) => ok(id, None),
                            Err(error) => failed(id, error),
                        },
                    );
                }
                Request::LogTail { max } => {
                    let (entries, dropped, seq) = self.eng.log_tail(max);
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::LogTail {
                                    entries,
                                    dropped,
                                    seq,
                                }),
                            }),
                        },
                    );
                }
                request => return Err(ClientMessage::Request { id, request }),
            },
            ClientMessage::Close {
                id,
                doc,
                discard_changes,
            } => {
                let own = match from {
                    Addr::Doc(doc) => doc,
                    Addr::App => None,
                };
                let Some(doc) = doc.or(own) else {
                    fail(
                        to,
                        id,
                        ErrorCode::NoWorkspace,
                        "close names no workspace and this connection is bound to none",
                    );
                    return Ok(Flow::Continue);
                };
                match self.remove_doc(doc, discard_changes) {
                    Ok(()) => {
                        reply(to, ok(id, None));
                        self.closed(doc);
                    }
                    Err(e) => reply(to, failed(id, e)),
                }
            }
            ClientMessage::Panic { id } => {
                self.panic_all();
                reply(to, ok(id, None));
            }
            ClientMessage::Hotkey {
                id,
                key,
                modifiers,
                phase,
            } => {
                let hotkey = openlab_core::Hotkey { key, modifiers };
                match phase {
                    HotkeyPhase::Down => self.hotkey(&hotkey),
                    HotkeyPhase::Up => self.hotkey_released(&hotkey),
                }
                reply(to, ok(id, None));
            }
            message => return Err(message),
        }
        Ok(Flow::Continue)
    }

    /// PB-10: Network cues of every open doc send nothing (or again).
    fn set_network_disabled(&mut self, disabled: bool) {
        self.eng.set_network_disabled(disabled);
    }
}

/// Who a client message comes from: a sender bound to a doc (`None`: it is closed, or none is
/// open for `Inbox::Client`), or an app connection.
#[derive(Clone, Copy, Debug)]
enum Addr {
    Doc(Option<DocId>),
    App,
}

/// The answer to a workspace message with no doc to act on (none open, the one it names is
/// closed, or an app connection sent it): `no_workspace`; a shutdown still shuts down.
fn no_doc(message: ClientMessage, to: &ReplyTx) -> Flow {
    let id = match message {
        ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
        ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => return Flow::Continue,
        ClientMessage::Transport { id, .. }
        | ClientMessage::Command { id, .. }
        | ClientMessage::Undo { id }
        | ClientMessage::Redo { id }
        | ClientMessage::Request { id, .. }
        | ClientMessage::Close { id, .. }
        | ClientMessage::Panic { id }
        | ClientMessage::Hotkey { id, .. } => id,
    };
    fail(to, id, ErrorCode::NoWorkspace, "no workspace is open");
    Flow::Continue
}

impl DocCx<'_> {
    fn client(&mut self, message: ClientMessage, to: &ReplyTx) -> Flow {
        let now = self.now();
        match message {
            ClientMessage::Transport { id, action } => {
                self.transport(action, now);
                reply(to, ok(id, None));
            }
            ClientMessage::Command {
                id,
                command,
                coalesce,
            } => {
                if coalesce
                    .as_ref()
                    .is_some_and(|k| k.len() > MAX_COALESCE_KEY)
                {
                    fail(
                        to,
                        id,
                        ErrorCode::BadRequest,
                        format!("coalesce key longer than {MAX_COALESCE_KEY} bytes"),
                    );
                    return Flow::Continue;
                }
                // Before the footprint: a raw paste becomes cues with ids only here.
                let command = match self.prepare_command(*command) {
                    Ok(command) => command,
                    Err(e) => {
                        reply(to, refused(id, &e));
                        return Flow::Continue;
                    }
                };
                let footprint = command.footprint();
                let mut edit = self.doc.model.edit();
                let result =
                    self.doc
                        .history
                        .execute_coalescing(&mut edit, command, coalesce.as_deref());
                edit.ended(&footprint, &result);
                match result {
                    Ok(changed) => {
                        // An empty transaction, or an edit that sets what is already there,
                        // changed nothing and is no undo step: no revision, no
                        // `model_changed`, not dirty.
                        if changed {
                            self.edited(now, &footprint);
                        }
                        reply(to, ok(id, Some(self.doc.revision)));
                    }
                    Err(e) => {
                        self.partly_changed(now, &e);
                        reply(to, refused(id, &e));
                    }
                }
            }
            ClientMessage::Undo { id } | ClientMessage::Redo { id } => {
                let undo = matches!(message, ClientMessage::Undo { .. });
                // The step about to be applied says what it changes.
                let footprint = if undo {
                    self.doc.history.peek_undo()
                } else {
                    self.doc.history.peek_redo()
                }
                .map(Command::footprint)
                .unwrap_or_default();
                // A step that removes an active cue (an undo of its add, a redo of its delete)
                // is performed, as QLab 5 does; the engine stops the cue as it stops a deleted
                // one when it follows the edit.
                let mut edit = self.doc.model.edit();
                let result = if undo {
                    self.doc.history.undo(&mut edit)
                } else {
                    self.doc.history.redo(&mut edit)
                };
                edit.ended(&footprint, &result);
                match result {
                    Ok(true) => {
                        self.edited(now, &footprint);
                        reply(to, ok(id, Some(self.doc.revision)));
                    }
                    Ok(false) => reply(to, ok(id, None)),
                    Err(e) => {
                        self.partly_changed(now, &e);
                        reply(to, refused(id, &e));
                    }
                }
            }
            ClientMessage::Request { id, request } => match request {
                Request::Snapshot => {
                    reply(
                        to,
                        ServerMessage::Snapshot {
                            snapshot: Box::new(self.doc.snapshot()),
                        },
                    );
                    reply(to, ok(id, None));
                }
                Request::SetShowMode { on } => {
                    if self.doc.show_mode != on {
                        self.doc.show_mode = on;
                        self.document();
                    }
                    reply(to, ok(id, None));
                }
                Request::ListPatchMappings => {
                    let workspace = self.doc.model.ws().id;
                    let patches = self
                        .doc
                        .model
                        .ws()
                        .patches
                        .iter()
                        .map(|p| PatchMappingEntry {
                            patch: p.id,
                            mapping: self
                                .eng
                                .config
                                .machine
                                .patch_mapping(workspace, p.id)
                                .cloned(),
                        })
                        .collect();
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::PatchMappings { patches }),
                            }),
                        },
                    );
                }
                Request::PreshowCheck => {
                    let report = self.preshow_report();
                    reply(
                        to,
                        ServerMessage::Result {
                            id,
                            outcome: Outcome::Ok(OkBody {
                                revision: None,
                                data: Some(ResultData::PreshowCheck(Box::new(report))),
                            }),
                        },
                    );
                }
                Request::Waveform {
                    cue,
                    start,
                    end,
                    buckets,
                } => self.waveform(id, cue, (start, end, buckets), to),
                // Saves come through [`Control::save`], which knows the other docs.
                request => self.doc_request(id, request, to, Vec::new()),
            },
            ClientMessage::Goodbye { shutdown: true } => return Flow::Shutdown,
            // The network layer handles the handshake and closing (M4.3).
            ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => {}
            // App-scope: [`Control::app_scope`] takes them before any doc does.
            ClientMessage::Close { id, .. }
            | ClientMessage::Panic { id }
            | ClientMessage::Hotkey { id, .. } => fail(
                to,
                id,
                ErrorCode::Internal,
                "an app-scope message reached a workspace",
            ),
        }
        Flow::Continue
    }
}
