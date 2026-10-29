// SPDX-License-Identifier: GPL-3.0-or-later

//! The WebSocket server: accept, authenticate, then relay between one client and the control
//! thread (docs/network.md, docs/protocol.md section 2).

use crate::lock;
use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use openlab_core::protocol::{
    ClientMessage, DocId, ErrorCode, LogLevel, MAX_CLIENT_FRAME_BYTES, Outcome, PROTOCOL_VERSION,
    ProtocolError, Request, RequestId, ServerMessage, decode_client, decode_hello, encode_server,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout, timeout_at};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

use super::NetError;
use super::hub::{self, Broadcast, Hub, Kind};
use super::osc::{self, OscConfig, OscListenerStats, OscPorts};
use super::refusals::Refusals;
use super::relaunch::{AppProcess, Budget, Next, Relaunch, next};
use super::stopping::Stopping;
use super::token::{constant_time_eq, generate_token};
use crate::control::{Inbox, OutboundRx, ReplyRx, ReplyTx, reply_channel};

/// Bytes a connection may send before it has authenticated: the HTTP upgrade request plus the
/// 4 KiB `hello`. The WebSocket library would buffer a whole message up to its size limit before
/// anyone looks at it, so this caps what a stranger can make the engine hold.
const PRE_AUTH_BYTES: usize = 16 * 1024;
/// Requests a connection may have outstanding at the control loop; it stops reading the socket
/// beyond that, so a flood is held back by TCP instead of piling up in the inbox.
const MAX_INFLIGHT: usize = 64;
/// Unreadable frames in a row before the connection is closed as abusive.
const MAX_BAD_FRAMES: u32 = 32;
/// Reserved `RequestId` for the server's own `snapshot` requests; a client may not use it.
const INTERNAL_ID: RequestId = RequestId(u64::MAX);
/// How long the control loop may take to produce the first snapshot.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(10);

/// Timeouts and limits. The defaults are for production; tests shorten them.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// From accepting the TCP connection to a valid `hello` (upgrade included).
    pub hello_timeout: Duration,
    /// A `ping` is sent this often.
    pub ping_interval: Duration,
    /// A client that has sent nothing (not even a `pong`) for this long is dead.
    pub dead_after: Duration,
    /// One write to a client that makes no progress for this long closes the connection.
    pub write_timeout: Duration,
    /// How long a `result` waits for the `model_changed` that precedes it.
    pub revision_wait: Duration,
    /// How long the connection holds broadcasts for a request that answers with a snapshot
    /// (`open` reads a file; the engine bounds that read itself, so this only covers an answer
    /// that is lost or very late). After it the hold is dropped and the connection resyncs.
    pub snapshot_wait: Duration,
    /// Authenticated connections.
    pub max_connections: usize,
    /// Connections still upgrading or waiting to say `hello`.
    pub max_handshaking: usize,
    /// Refused connections are logged at most this often (with a count), so a process that
    /// keeps knocking cannot flood the log.
    pub refusal_log_interval: Duration,
    /// The OSC listeners (docs/osc.md), on this server's runtime. `None`: no OSC.
    pub osc: Option<OscConfig>,
    /// What to do when an app connection ends without `goodbye` (the UI crashed; M4.9).
    /// `None`: only log it.
    pub relaunch: Option<Relaunch>,
    /// Set when the engine's shutdown begins (by a `goodbye { shutdown: true }` here, or by the
    /// host): an app connection that ends after it is not relaunched.
    pub stopping: Stopping,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(10),
            dead_after: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            revision_wait: Duration::from_secs(2),
            snapshot_wait: Duration::from_secs(60),
            // Two per open workspace (its window and a spare) plus the app connections, with
            // room for M4.15's 8 workspaces.
            max_connections: 32,
            max_handshaking: 8,
            refusal_log_interval: Duration::from_secs(10),
            osc: None,
            relaunch: None,
            stopping: Stopping::default(),
        }
    }
}

struct Shared {
    token: String,
    inbox: Sender<Inbox>,
    hub: Arc<Hub>,
    config: ServerConfig,
    handshaking: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
    relaunches: Mutex<Budget>,
    /// The app process the latest app connection's `hello` named (relaunch.rs); `None`: it
    /// named none, or no relaunch is configured.
    app: Mutex<Option<AppProcess>>,
}

/// The running server. Dropping it (or [`WsServer::stop`]) closes every connection.
pub struct WsServer {
    port: u16,
    token: String,
    shutdown: watch::Sender<bool>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    osc: Option<osc::Running>,
}

impl std::fmt::Debug for WsServer {
    // The token is a secret: never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsServer")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl WsServer {
    /// Listens on `127.0.0.1` at a random port with a fresh random token. Client messages go to
    /// `inbox`; everything on `outbound` (the control loop's broadcast channel) is relayed to
    /// the connected clients.
    pub fn start(
        inbox: Sender<Inbox>,
        outbound: OutboundRx,
        config: ServerConfig,
    ) -> Result<Self, NetError> {
        let token = generate_token()?;
        let listener = StdListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(NetError::Bind)?;
        listener.set_nonblocking(true).map_err(NetError::Bind)?;
        let port = listener.local_addr().map_err(NetError::Bind)?.port();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("openlab-net")
            .enable_io()
            .enable_time()
            .build()
            .map_err(NetError::Runtime)?;
        // OSC is optional and must never keep the engine from starting: a port in use (another
        // QLab-compatible app) is a warning in the log, and the UI protocol still works.
        let osc_bound = config
            .osc
            .clone()
            .and_then(|osc_config| match osc::bind(&osc_config) {
                Ok(bound) => Some((bound, osc_config)),
                Err(e) => {
                    let _ = inbox.send(Inbox::Log {
                        level: LogLevel::Warn,
                        message: format!(
                            "OSC server is off: could not listen on {}:{}: {e}",
                            osc_config.bind, osc_config.port
                        ),
                    });
                    None
                }
            });
        let (osc, tap) = match osc_bound {
            Some((bound, osc_config)) => {
                match osc::start(&runtime, bound, osc_config, inbox.clone()) {
                    Some((running, tap)) => (Some(running), Some(tap)),
                    None => (None, None),
                }
            }
            None => (None, None),
        };
        let (shutdown, shutdown_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            token: token.clone(),
            inbox,
            hub: Arc::new(Hub::default()),
            handshaking: Arc::new(Semaphore::new(config.max_handshaking)),
            connections: Arc::new(Semaphore::new(config.max_connections)),
            config,
            shutdown: shutdown_rx,
            relaunches: Mutex::new(Budget::default()),
            app: Mutex::new(None),
        });
        runtime.spawn(hub::distribute(shared.hub.clone(), outbound, tap));
        let accept_shared = shared.clone();
        runtime.spawn(async move {
            if let Ok(listener) = TcpListener::from_std(listener) {
                accept_loop(accept_shared, listener).await;
            }
        });
        Ok(Self {
            port,
            token,
            shutdown,
            runtime: Mutex::new(Some(runtime)),
            osc,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The OSC sockets' ports; `None` if OSC is off or could not bind.
    pub fn osc_ports(&self) -> Option<OscPorts> {
        self.osc.as_ref().map(|o| o.ports)
    }

    /// What the OSC listeners dropped, refused or answered; `None` if OSC is off.
    pub fn osc_stats(&self) -> Option<OscListenerStats> {
        self.osc.as_ref().map(osc::Running::stats)
    }

    /// The token a client must present in `hello` (written to `engine.json` by M4.9).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Closes every connection and stops the runtime (and with it the fan-out task).
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
        let runtime = lock(&self.runtime).take();
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    let mut shutdown = shared.shutdown.clone();
    let mut refusals = Refusals::new("UI server", shared.config.refusal_log_interval);
    loop {
        let due = refusals.due();
        let accepted = tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => accepted,
            // The last refusals of a burst are logged even if no connection follows.
            () = sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                refusals.flush(&shared.inbox);
                continue;
            }
        };
        let Ok((tcp, _)) = accepted else {
            // A transient accept failure (descriptor exhaustion): do not spin.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        // Over the limits: refuse by closing at once, before any state is allocated.
        let Ok(pending) = shared.handshaking.clone().try_acquire_owned() else {
            refusals.refused(&shared.inbox, "too many connections still handshaking");
            continue;
        };
        let Ok(slot) = shared.connections.clone().try_acquire_owned() else {
            refusals.refused(&shared.inbox, "connection limit reached");
            continue;
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            serve(shared, tcp, pending).await;
            drop(slot);
        });
    }
}

/// The socket, with a cap on what an unauthenticated peer may send (see [`PRE_AUTH_BYTES`]).
struct Gated {
    inner: TcpStream,
    read: usize,
    authed: Arc<AtomicBool>,
}

impl AsyncRead for Gated {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled
            && !self.authed.load(Ordering::Acquire)
        {
            self.read += buf.filled().len().saturating_sub(before);
            if self.read > PRE_AUTH_BYTES {
                return Poll::Ready(Err(io::Error::other("too much data before hello")));
            }
        }
        polled
    }
}

impl AsyncWrite for Gated {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

type Ws = WebSocketStream<Gated>;

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_CLIENT_FRAME_BYTES))
        .max_frame_size(Some(MAX_CLIENT_FRAME_BYTES))
}

fn error_message(code: ErrorCode, message: &str) -> ServerMessage {
    ServerMessage::Error {
        code,
        message: message.to_owned(),
        protocol_version: PROTOCOL_VERSION,
    }
}

/// Checks the first frame: the workspace the connection binds to (`None`: an app connection).
/// `Err` is the refusal to send before closing.
/// The doc the connection is bound to and the pid the client gave.
fn authenticate(
    shared: &Shared,
    text: &str,
) -> Result<(Option<DocId>, Option<u32>), ServerMessage> {
    match decode_hello(text) {
        Ok(ClientMessage::Hello {
            protocol_version,
            token,
            workspace,
            pid,
            ..
        }) => {
            if !constant_time_eq(token.as_bytes(), shared.token.as_bytes()) {
                Err(error_message(ErrorCode::BadToken, "wrong token"))
            } else if protocol_version != PROTOCOL_VERSION {
                Err(error_message(
                    ErrorCode::ProtocolVersionMismatch,
                    &format!("the engine speaks protocol {PROTOCOL_VERSION}"),
                ))
            } else {
                Ok((workspace, pid))
            }
        }
        // Any other message first (docs/protocol.md section 2).
        Ok(_) | Err(ProtocolError::UnknownType(_)) => Err(error_message(
            ErrorCode::BadToken,
            "the first message must be hello",
        )),
        Err(e) => Err(error_message(ErrorCode::BadRequest, &e.to_string())),
    }
}

async fn serve(shared: Arc<Shared>, tcp: TcpStream, pending: OwnedSemaphorePermit) {
    let _ = tcp.set_nodelay(true);
    let authed = Arc::new(AtomicBool::new(false));
    let stream = Gated {
        inner: tcp,
        read: 0,
        authed: authed.clone(),
    };
    let deadline = Instant::now() + shared.config.hello_timeout;
    let Ok(Ok(mut ws)) = timeout_at(
        deadline,
        accept_async_with_config(stream, Some(ws_config())),
    )
    .await
    else {
        return;
    };
    let hello = match timeout_at(deadline, ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        _ => return,
    };
    let doc = match authenticate(&shared, hello.as_str()) {
        Ok((doc, pid)) => {
            if doc.is_none()
                && let Some(relaunch) = &shared.config.relaunch
            {
                *lock(&shared.app) = relaunch.identify(pid);
            }
            doc
        }
        Err(refusal) => {
            if let Ok(text) = encode_server(&refusal) {
                let _ = timeout(shared.config.write_timeout, ws.send(Message::text(text))).await;
            }
            let _ = timeout(shared.config.write_timeout, ws.close(None)).await;
            return;
        }
    };
    authed.store(true, Ordering::Release);
    drop(pending);
    run(shared, ws, doc).await;
}

/// The connection ended (or must end).
struct Closed;

/// A `result` waiting for the `model_changed` it reports (docs/protocol.md section 4).
struct Held {
    revision: u64,
    deadline: Instant,
    message: ServerMessage,
}

struct Conn {
    ws: Ws,
    shared: Arc<Shared>,
    /// The doc this connection is bound to; `None`: the app connection.
    doc: Option<DocId>,
    hub_id: u64,
    bcast: mpsc::Receiver<Broadcast>,
    lagged: Arc<AtomicBool>,
    wake: Arc<Notify>,
    reply_tx: ReplyTx,
    reply_rx: ReplyRx,
    /// Last `model_changed` revision / `runtime` seq this client has been sent or was told by a
    /// snapshot; older broadcasts are skipped.
    last_rev: u64,
    last_seq: u64,
    /// Client requests sent to the control loop whose `result` has not been written yet.
    inflight: usize,
    /// Requests that answer with a `snapshot` (the server's own, id [`INTERNAL_ID`], and the
    /// client's `snapshot`; on an app connection the `workspaces` list) whose `result` has not
    /// arrived. While one is
    /// out, broadcasts wait in the queue: one made after the control loop took the snapshot
    /// must not reach the client before it (the snapshot would then take the client back),
    /// and one made before is filtered against the snapshot's revision and seq.
    snapshot_waits: Vec<(RequestId, Instant)>,
    /// Results held back for their `model_changed`, in arrival order.
    held: VecDeque<Held>,
    bad_frames: u32,
    last_heard: Instant,
    /// The client said `goodbye`: its end is no crash.
    said_goodbye: bool,
}

async fn run(shared: Arc<Shared>, ws: Ws, doc: Option<DocId>) {
    let registration = shared.hub.register(doc);
    let (reply_tx, reply_rx) = reply_channel();
    let mut conn = Conn {
        ws,
        doc,
        hub_id: registration.id,
        bcast: registration.rx,
        lagged: registration.lagged,
        wake: registration.wake,
        reply_tx,
        reply_rx,
        last_rev: 0,
        last_seq: 0,
        inflight: 0,
        snapshot_waits: Vec::new(),
        held: VecDeque::new(),
        bad_frames: 0,
        last_heard: Instant::now(),
        said_goodbye: false,
        shared,
    };
    let _ = conn.session().await;
    conn.shared.hub.unregister(conn.hub_id);
    if conn.doc.is_none() && !conn.said_goodbye && !*conn.shared.shutdown.borrow() {
        app_lost(&conn.shared);
    }
    let _ = timeout(conn.shared.config.write_timeout, conn.ws.close(None)).await;
}

/// Logged instead of a relaunch once the engine's shutdown has begun.
const NOT_RELAUNCHED_STOPPING: &str =
    "the app's connection closed while the engine shuts down; the app is not relaunched";

/// An app connection ended without `goodbye` (relaunch.rs): log it, and relaunch the app after
/// the delay unless one is back by then or the engine has begun to shut down (relaunching the
/// app onto an engine about to exit would only leave it to find the engine gone).
fn app_lost(shared: &Arc<Shared>) {
    let log = |shared: &Shared, level, message: &str| {
        let _ = shared.inbox.send(Inbox::Log {
            level,
            message: message.to_owned(),
        });
    };
    if shared.config.stopping.is_set() {
        log(shared, LogLevel::Info, NOT_RELAUNCHED_STOPPING);
        return;
    }
    log(
        shared,
        LogLevel::Warn,
        "the app's connection closed without goodbye (did it crash?); playback goes on",
    );
    let Some(relaunch) = shared.config.relaunch.clone() else {
        return;
    };
    if !lock(&shared.relaunches).begin() {
        return;
    }
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let mut said_waiting = false;
        loop {
            tokio::time::sleep(relaunch.delay).await;
            let app = lock(&shared.app).clone();
            let alive = app.as_ref().is_some_and(|a| relaunch.alive(a));
            let decision = next(
                shared.hub.app_connections() > 0,
                *shared.shutdown.borrow(),
                shared.config.stopping.is_set(),
                alive,
            );
            match decision {
                Next::Done => {
                    lock(&shared.relaunches).end();
                    return;
                }
                Next::Stopping => {
                    lock(&shared.relaunches).end();
                    log(&shared, LogLevel::Info, NOT_RELAUNCHED_STOPPING);
                    return;
                }
                Next::Wait => {
                    if !said_waiting {
                        said_waiting = true;
                        let pid = app.map_or(0, |a| a.pid);
                        log(
                            &shared,
                            LogLevel::Warn,
                            &format!(
                                "the app (pid {pid}) is still running: waiting for it to \
                                 reconnect; it is relaunched if it quits"
                            ),
                        );
                    }
                }
                Next::Relaunch => break,
            }
        }
        let allowed = lock(&shared.relaunches).take(Instant::now());
        lock(&shared.relaunches).end();
        if !allowed {
            log(
                &shared,
                LogLevel::Error,
                "the app is not relaunched: it was relaunched 3 times in the last minute",
            );
            return;
        }
        log(&shared, LogLevel::Warn, "relaunching the app");
        let action = relaunch.action;
        match tokio::task::spawn_blocking(move || action()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log(
                &shared,
                LogLevel::Error,
                &format!("could not relaunch the app: {e}"),
            ),
            Err(_) => log(&shared, LogLevel::Error, "could not relaunch the app"),
        }
    });
}

fn request_id(message: &ClientMessage) -> Option<RequestId> {
    match message {
        ClientMessage::Command { id, .. }
        | ClientMessage::Transport { id, .. }
        | ClientMessage::Undo { id }
        | ClientMessage::Redo { id }
        | ClientMessage::Request { id, .. }
        | ClientMessage::Close { id, .. }
        | ClientMessage::Panic { id }
        | ClientMessage::Hotkey { id, .. } => Some(*id),
        ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => None,
    }
}

/// Whether the control loop answers this with a `snapshot` (docs/protocol.md section 3; the
/// `workspaces` list on an app connection).
fn answers_with_snapshot(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Request {
            request: Request::Snapshot,
            ..
        }
    )
}

/// A transport action, the app-scope panic or a hotkey: a trigger the show log records.
fn is_trigger(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::Transport { .. }
            | ClientMessage::Panic { .. }
            | ClientMessage::Hotkey { .. }
    )
}

impl Conn {
    async fn session(&mut self) -> Result<(), Closed> {
        // The hub is registered before the snapshot is taken, so a change in between is
        // queued; the revision filter drops what the snapshot already contains. A connection
        // bound to a doc that is not open is refused (`no_workspace`): the doc's `snapshot`
        // answers that, and a doc closed after it gives the connection `closed`.
        self.ask_snapshot()?;
        let welcome = loop {
            match timeout(SNAPSHOT_WAIT, self.reply_rx.recv()).await {
                Ok(Some(ServerMessage::Snapshot { snapshot })) => {
                    self.last_rev = snapshot.revision;
                    self.last_seq = snapshot.runtime_seq;
                    break ServerMessage::Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        doc: self.doc,
                        snapshot: Some(snapshot),
                        workspaces: None,
                    };
                }
                Ok(Some(ServerMessage::Workspaces { workspaces })) => {
                    break ServerMessage::Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        doc: None,
                        snapshot: None,
                        workspaces: Some(workspaces),
                    };
                }
                Ok(Some(ServerMessage::Result {
                    id: INTERNAL_ID,
                    outcome: Outcome::Error(e),
                })) => {
                    let _ = self.send(&error_message(e.code, &e.message)).await;
                    return Err(Closed);
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = self
                        .send(&error_message(
                            ErrorCode::Internal,
                            "the engine did not produce a snapshot",
                        ))
                        .await;
                    return Err(Closed);
                }
            }
        };
        self.send(&welcome).await?;

        let mut shutdown = self.shared.shutdown.clone();
        let wake = self.wake.clone();
        let config = self.shared.config.clone();
        let mut ticker = interval_at(Instant::now() + config.ping_interval, config.ping_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        self.last_heard = Instant::now();
        loop {
            let waiting = !self.snapshot_waits.is_empty();
            if !waiting && self.lagged.load(Ordering::Acquire) {
                self.resync()?;
                continue;
            }
            // While a snapshot is coming, held results wait for it (it covers their revision:
            // the control loop answered them before taking it).
            let deadline = self
                .held
                .iter()
                .map(|h| h.deadline)
                .min()
                .filter(|_| !waiting);
            let wait_deadline = self
                .snapshot_waits
                .iter()
                .map(|(_, since)| *since + config.snapshot_wait)
                .min();
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                // Only wakes the loop; `resync` runs at the top of the next turn.
                () = wake.notified() => {}
                // Broadcasts before replies: the `model_changed` of a command is queued here
                // before its `result` is ready (ordering also enforced by `held`).
                item = self.bcast.recv(), if !waiting => match item {
                    Some(b) => self.broadcast(b).await?,
                    None => return Ok(()),
                },
                reply = self.reply_rx.recv() => match reply {
                    Some(m) => self.reply(m).await?,
                    None => return Ok(()),
                },
                frame = self.ws.next(), if self.inflight < MAX_INFLIGHT => {
                    self.last_heard = Instant::now();
                    match frame {
                        None | Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => {
                            if matches!(e, WsError::Capacity(_)) {
                                let _ = self
                                    .send(&error_message(ErrorCode::BadRequest, "frame too large"))
                                    .await;
                            }
                            return Err(Closed);
                        }
                        Some(Ok(Message::Text(text))) => self.text(text.as_str()).await?,
                        Some(Ok(Message::Binary(_))) => {
                            self.bad_frame(ErrorCode::BadRequest, "binary frames are not part of the protocol")
                                .await?;
                        }
                        // Ping is answered by the library; pong only counts as sign of life.
                        Some(Ok(_)) => {}
                    }
                }
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.release_held().await?;
                }
                () = sleep_until(wait_deadline.unwrap_or_else(Instant::now)), if wait_deadline.is_some() => {
                    self.expire_waits();
                }
                _ = ticker.tick() => {
                    if self.inflight < MAX_INFLIGHT
                        && self.last_heard.elapsed() > config.dead_after
                    {
                        return Err(Closed);
                    }
                    self.write(Message::Ping(Default::default())).await?;
                }
            }
        }
    }

    /// A snapshot that never came (or came very late): stop holding broadcasts for it. The queue
    /// may have overflowed meanwhile, so the connection takes a fresh snapshot to be sure.
    fn expire_waits(&mut self) {
        let (now, limit) = (Instant::now(), self.shared.config.snapshot_wait);
        let before = self.snapshot_waits.len();
        self.snapshot_waits
            .retain(|(_, since)| now.duration_since(*since) < limit);
        if self.snapshot_waits.len() != before {
            self.lagged.store(true, Ordering::Release);
        }
    }

    fn ask_snapshot(&mut self) -> Result<(), Closed> {
        self.snapshot_waits.push((INTERNAL_ID, Instant::now()));
        self.to_control(ClientMessage::Request {
            id: INTERNAL_ID,
            request: Request::Snapshot,
        })
    }

    /// Hands a message to the control loop, for the doc this connection is bound to.
    fn to_control(&self, message: ClientMessage) -> Result<(), Closed> {
        self.shared
            .inbox
            .send(Inbox::Conn {
                doc: self.doc,
                message,
                reply: self.reply_tx.clone(),
            })
            .map_err(|_| Closed)
    }

    /// The doc was closed: what the control loop answered before it closed is written (its
    /// replies are in the reply channel already: they are sent before the broadcast), then
    /// `closed`, and the connection ends.
    async fn doc_closed(&mut self, closed: &str) -> Result<(), Closed> {
        // Nothing is held back any more: no later `model_changed` of this doc will come.
        while let Some(h) = self.held.pop_front() {
            self.send(&h.message).await?;
        }
        while let Ok(m) = self.reply_rx.try_recv() {
            self.send(&m).await?;
        }
        self.send_text(closed).await?;
        Err(Closed)
    }

    /// The client's queue overflowed (or a message could not be encoded): throw the queue away
    /// and send a fresh snapshot. The flag is cleared first, so a broadcast made after this
    /// point is queued and filtered against the snapshot's revision.
    fn resync(&mut self) -> Result<(), Closed> {
        while self.bcast.try_recv().is_ok() {}
        self.lagged.store(false, Ordering::Release);
        self.ask_snapshot()
    }

    async fn write(&mut self, message: Message) -> Result<(), Closed> {
        match timeout(self.shared.config.write_timeout, self.ws.send(message)).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(Closed),
        }
    }

    async fn send_text(&mut self, text: &str) -> Result<(), Closed> {
        self.write(Message::text(text)).await
    }

    async fn send(&mut self, message: &ServerMessage) -> Result<(), Closed> {
        match encode_server(message) {
            Ok(text) => self.send_text(&text).await,
            Err(_) => {
                // Only a snapshot can be this big: the client cannot be served.
                let refusal = error_message(ErrorCode::Internal, "message too large to send");
                if let Ok(text) = encode_server(&refusal) {
                    let _ = self.send_text(&text).await;
                }
                Err(Closed)
            }
        }
    }

    async fn broadcast(&mut self, b: Broadcast) -> Result<(), Closed> {
        match b.kind {
            Kind::Revision(r) => {
                if r <= self.last_rev {
                    return Ok(());
                }
                self.last_rev = r;
            }
            Kind::Seq(s) => {
                if s <= self.last_seq {
                    return Ok(());
                }
                self.last_seq = s;
                return self.send_text(&b.text).await;
            }
            Kind::Snapshot { revision, seq } => {
                // The requester already has it as its reply (a snapshot from the same
                // numbering, or a later one); everyone else takes it and restarts its
                // filters from it, like any snapshot.
                if revision <= self.last_rev && seq <= self.last_seq {
                    return Ok(());
                }
                self.last_rev = revision;
                self.last_seq = seq;
                self.send_text(&b.text).await?;
                return self.release_held().await;
            }
            Kind::Closed => return self.doc_closed(&b.text).await,
            Kind::State | Kind::Lossy => return self.send_text(&b.text).await,
        }
        self.send_text(&b.text).await?;
        self.release_held().await
    }

    /// Writes the held results whose `model_changed` has been written (or covered by a
    /// snapshot), and, unless a snapshot is coming, those that waited `revision_wait`: their
    /// `model_changed` was lost (the control loop's outbound channel was full) and the client,
    /// seeing `result.revision` ahead of its own, asks for a snapshot.
    async fn release_held(&mut self) -> Result<(), Closed> {
        let now = Instant::now();
        let timed_out_ok = self.snapshot_waits.is_empty();
        while let Some(i) = self
            .held
            .iter()
            .position(|h| h.revision <= self.last_rev || (timed_out_ok && h.deadline <= now))
        {
            if let Some(h) = self.held.remove(i) {
                self.inflight = self.inflight.saturating_sub(1);
                self.send(&h.message).await?;
            }
        }
        Ok(())
    }

    async fn reply(&mut self, message: ServerMessage) -> Result<(), Closed> {
        match &message {
            ServerMessage::Snapshot { snapshot } => {
                if snapshot.revision < self.last_rev || snapshot.runtime_seq < self.last_seq {
                    // Behind what the client was already sent: stale (a client reusing an id
                    // can lift the wait early) or the numbering restarted. Either way a fresh
                    // snapshot follows at once and settles it. Not dropped: if the numbering
                    // restarted, the fresh one would be "behind" too and never get through.
                    self.lagged.store(true, Ordering::Release);
                }
                self.last_rev = snapshot.revision;
                self.last_seq = snapshot.runtime_seq;
                self.send(&message).await?;
                return self.release_held().await;
            }
            ServerMessage::Result { id, outcome } => {
                if let Some(i) = self.snapshot_waits.iter().position(|(w, _)| w == id) {
                    self.snapshot_waits.swap_remove(i);
                    if *id == INTERNAL_ID {
                        // A resync of a doc closed meanwhile (its `closed` was thrown away
                        // with the queue): the connection ends as it would have.
                        if let (Some(doc), Outcome::Error(_)) = (self.doc, outcome)
                            && let Ok(text) = encode_server(&ServerMessage::Closed { doc })
                        {
                            return self.doc_closed(&text).await;
                        }
                        return self.release_held().await;
                    }
                }
                if let Outcome::Ok(ok) = outcome
                    && let Some(revision) = ok.revision
                    && revision > self.last_rev
                {
                    self.held.push_back(Held {
                        revision,
                        deadline: Instant::now() + self.shared.config.revision_wait,
                        message,
                    });
                    return Ok(());
                }
                self.inflight = self.inflight.saturating_sub(1);
            }
            _ => {}
        }
        self.send(&message).await
    }

    async fn bad_frame(&mut self, code: ErrorCode, text: &str) -> Result<(), Closed> {
        self.bad_frames += 1;
        self.send(&error_message(code, text)).await?;
        if self.bad_frames >= MAX_BAD_FRAMES {
            let _ = self
                .send(&error_message(
                    ErrorCode::BadRequest,
                    "too many unreadable frames",
                ))
                .await;
            return Err(Closed);
        }
        Ok(())
    }

    async fn text(&mut self, text: &str) -> Result<(), Closed> {
        let message = match decode_client(text) {
            Ok(message) => message,
            Err(e) => {
                let code = match e {
                    ProtocolError::UnknownType(_) => ErrorCode::UnknownMessage,
                    _ => ErrorCode::BadRequest,
                };
                return self.bad_frame(code, &e.to_string()).await;
            }
        };
        self.bad_frames = 0;
        match message {
            ClientMessage::Hello { .. } => {
                self.send(&error_message(ErrorCode::BadRequest, "already connected"))
                    .await
            }
            ClientMessage::Goodbye { shutdown } => {
                self.said_goodbye = true;
                if shutdown {
                    // First, so the app connections that end from here on are no crash.
                    self.shared.config.stopping.begin();
                    // The control loop stops the show and ends (docs/network.md); the process
                    // exit is the host's.
                    let _ = self.to_control(ClientMessage::Goodbye { shutdown: true });
                }
                Err(Closed)
            }
            other => {
                let Some(id) = request_id(&other) else {
                    return Ok(());
                };
                if id == INTERNAL_ID {
                    return self
                        .send(&error_message(
                            ErrorCode::BadRequest,
                            "request id is reserved",
                        ))
                        .await;
                }
                // Before the send: the control loop may answer at once.
                if answers_with_snapshot(&other) {
                    self.snapshot_waits.push((id, Instant::now()));
                }
                self.inflight += 1;
                // Edits and requests are the UI's own business; a transport action, the panic
                // and a hotkey are triggers the show log records (DX-02).
                if is_trigger(&other) {
                    let _ = self.shared.inbox.send(Inbox::Trigger {
                        source: "ui".to_owned(),
                        detail: super::describe(&other),
                    });
                }
                if self.to_control(other).is_err() {
                    let _ = self
                        .send(&error_message(
                            ErrorCode::Internal,
                            "the engine is shutting down",
                        ))
                        .await;
                    return Err(Closed);
                }
                Ok(())
            }
        }
    }
}
