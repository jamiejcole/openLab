// SPDX-License-Identifier: GPL-3.0-or-later

//! The WebSocket server: accept, authenticate, then relay between one client and the control
//! thread (docs/network.md, docs/protocol.md section 2).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use openlab_core::protocol::{
    ClientMessage, ErrorCode, MAX_CLIENT_FRAME_BYTES, Outcome, PROTOCOL_VERSION, ProtocolError,
    Request, RequestId, ServerMessage, decode_client, decode_hello, encode_server,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, timeout, timeout_at};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

use super::NetError;
use super::hub::{self, Broadcast, Hub, Kind};
use super::token::{constant_time_eq, generate_token};
use crate::control::Inbox;

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
    /// Authenticated connections.
    pub max_connections: usize,
    /// Connections still upgrading or waiting to say `hello`.
    pub max_handshaking: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(10),
            dead_after: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            revision_wait: Duration::from_secs(2),
            max_connections: 16,
            max_handshaking: 8,
        }
    }
}

struct Shared {
    token: String,
    inbox: Sender<Inbox>,
    hub: Hub,
    config: ServerConfig,
    handshaking: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
}

/// The running server. Dropping it (or [`WsServer::stop`]) closes every connection.
pub struct WsServer {
    port: u16,
    token: String,
    shared: Arc<Shared>,
    stop_flag: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    distributor: Mutex<Option<JoinHandle<()>>>,
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
        outbound: Receiver<ServerMessage>,
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
        let (shutdown, shutdown_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            token: token.clone(),
            inbox,
            hub: Hub::default(),
            handshaking: Arc::new(Semaphore::new(config.max_handshaking)),
            connections: Arc::new(Semaphore::new(config.max_connections)),
            config,
            shutdown: shutdown_rx,
        });
        let stop_flag = Arc::new(AtomicBool::new(false));
        let distributor = {
            let shared = shared.clone();
            let stop = stop_flag.clone();
            thread::Builder::new()
                .name("openlab-net-fanout".into())
                .spawn(move || hub::distribute(&shared.hub, &outbound, &stop))
                .map_err(NetError::Thread)?
        };
        let accept_shared = shared.clone();
        runtime.spawn(async move {
            if let Ok(listener) = TcpListener::from_std(listener) {
                accept_loop(accept_shared, listener).await;
            }
        });
        Ok(Self {
            port,
            token,
            shared,
            stop_flag,
            shutdown,
            runtime: Mutex::new(Some(runtime)),
            distributor: Mutex::new(Some(distributor)),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The token a client must present in `hello` (written to `engine.json` by M4.9).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Closes every connection and stops the threads.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
        self.stop_flag.store(true, Ordering::Release);
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown_background();
        }
        let distributor = self
            .distributor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = distributor {
            let _ = handle.join();
        }
        let _ = &self.shared;
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    let mut shutdown = shared.shutdown.clone();
    loop {
        let accepted = tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _)) = accepted else {
            // A transient accept failure (descriptor exhaustion): do not spin.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        // Over the limits: refuse by closing at once, before any state is allocated.
        let Ok(pending) = shared.handshaking.clone().try_acquire_owned() else {
            continue;
        };
        let Ok(slot) = shared.connections.clone().try_acquire_owned() else {
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

/// Checks the first frame. `Err` is the refusal to send before closing.
fn authenticate(shared: &Shared, text: &str) -> Result<(), ServerMessage> {
    match decode_hello(text) {
        Ok(ClientMessage::Hello {
            protocol_version,
            token,
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
                Ok(())
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
    if let Err(refusal) = authenticate(&shared, hello.as_str()) {
        if let Ok(text) = encode_server(&refusal) {
            let _ = timeout(shared.config.write_timeout, ws.send(Message::text(text))).await;
        }
        let _ = timeout(shared.config.write_timeout, ws.close(None)).await;
        return;
    }
    authed.store(true, Ordering::Release);
    drop(pending);
    run(shared, ws).await;
}

/// The connection ended (or must end).
struct Closed;

struct Conn {
    ws: Ws,
    shared: Arc<Shared>,
    hub_id: u64,
    bcast: mpsc::Receiver<Broadcast>,
    lagged: Arc<AtomicBool>,
    wake: Arc<Notify>,
    reply_tx: Sender<ServerMessage>,
    reply_rx: mpsc::UnboundedReceiver<ServerMessage>,
    /// Last `model_changed` revision / `runtime` seq this client has been sent or was told by a
    /// snapshot; older broadcasts are skipped.
    last_rev: u64,
    last_seq: u64,
    inflight: usize,
    /// Own `snapshot` requests not yet answered. Broadcasts wait while one is out.
    internal_pending: usize,
    bad_frames: u32,
    last_heard: Instant,
    closed: Arc<AtomicBool>,
}

async fn run(shared: Arc<Shared>, ws: Ws) {
    let registration = shared.hub.register();
    let (reply_tx, reply_rx_c) = crossbeam_channel::unbounded();
    let (fw_tx, reply_rx) = mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let forwarder = {
        let closed = closed.clone();
        thread::Builder::new()
            .name("openlab-net-reply".into())
            .spawn(move || forward_replies(&reply_rx_c, &fw_tx, &closed))
    };
    let mut conn = Conn {
        ws,
        hub_id: registration.id,
        bcast: registration.rx,
        lagged: registration.lagged,
        wake: registration.wake,
        reply_tx,
        reply_rx,
        last_rev: 0,
        last_seq: 0,
        inflight: 0,
        internal_pending: 0,
        bad_frames: 0,
        last_heard: Instant::now(),
        closed,
        shared,
    };
    if forwarder.is_err() {
        let _ = conn
            .send(&error_message(
                ErrorCode::Internal,
                "engine is out of threads",
            ))
            .await;
    } else {
        let _ = conn.session().await;
    }
    conn.closed.store(true, Ordering::Release);
    conn.shared.hub.unregister(conn.hub_id);
    let _ = timeout(conn.shared.config.write_timeout, conn.ws.close(None)).await;
}

/// Moves replies from the control loop's crossbeam channel into the connection's task. One
/// small thread per connection (a few at most): crossbeam cannot wake a tokio task.
fn forward_replies(
    rx: &Receiver<ServerMessage>,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    closed: &AtomicBool,
) {
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(message) => {
                if tx.send(message).is_err() {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if closed.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn request_id(message: &ClientMessage) -> Option<RequestId> {
    match message {
        ClientMessage::Command { id, .. }
        | ClientMessage::Transport { id, .. }
        | ClientMessage::Undo { id }
        | ClientMessage::Redo { id }
        | ClientMessage::Request { id, .. } => Some(*id),
        ClientMessage::Hello { .. } | ClientMessage::Goodbye { .. } => None,
    }
}

impl Conn {
    async fn session(&mut self) -> Result<(), Closed> {
        // The hub is registered before the snapshot is taken, so a change in between is
        // queued; the revision filter drops what the snapshot already contains.
        self.ask_snapshot()?;
        let snapshot = loop {
            match timeout(SNAPSHOT_WAIT, self.reply_rx.recv()).await {
                Ok(Some(ServerMessage::Snapshot { snapshot })) => break snapshot,
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
        self.last_rev = snapshot.revision;
        self.last_seq = snapshot.runtime_seq;
        self.send(&ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            snapshot,
        })
        .await?;

        let mut shutdown = self.shared.shutdown.clone();
        let wake = self.wake.clone();
        let config = self.shared.config.clone();
        let mut ticker = interval_at(Instant::now() + config.ping_interval, config.ping_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        self.last_heard = Instant::now();
        loop {
            if self.internal_pending == 0 && self.lagged.load(Ordering::Acquire) {
                self.resync()?;
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                // Only wakes the loop; `resync` runs at the top of the next turn.
                () = wake.notified() => {}
                // Broadcasts before replies: the `model_changed` of a command is queued here
                // before its `result` is ready (ordering also enforced in `reply`).
                item = self.bcast.recv(), if self.internal_pending == 0 => match item {
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

    fn ask_snapshot(&mut self) -> Result<(), Closed> {
        self.internal_pending += 1;
        self.shared
            .inbox
            .send(Inbox::Client {
                message: ClientMessage::Request {
                    id: INTERNAL_ID,
                    request: Request::Snapshot,
                },
                reply: self.reply_tx.clone(),
            })
            .map_err(|_| Closed)
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
            }
            Kind::State | Kind::Lossy => {}
        }
        self.send_text(&b.text).await
    }

    /// Holds a `result` back until the `model_changed` it reports has been sent (docs/protocol.md
    /// section 4), unless that message was lost (client lagged, or the control loop's outbound
    /// channel was full): then it goes after `revision_wait` and the client, seeing
    /// `result.revision` ahead of its own, asks for a snapshot.
    async fn catch_up(&mut self, revision: u64) -> Result<(), Closed> {
        let deadline = Instant::now() + self.shared.config.revision_wait;
        while self.last_rev < revision
            && self.internal_pending == 0
            && !self.lagged.load(Ordering::Acquire)
        {
            match timeout_at(deadline, self.bcast.recv()).await {
                Ok(Some(b)) => self.broadcast(b).await?,
                Ok(None) | Err(_) => break,
            }
        }
        Ok(())
    }

    async fn reply(&mut self, message: ServerMessage) -> Result<(), Closed> {
        match &message {
            ServerMessage::Snapshot { snapshot } => {
                self.last_rev = snapshot.revision;
                self.last_seq = snapshot.runtime_seq;
            }
            ServerMessage::Result { id, .. } if *id == INTERNAL_ID => {
                self.internal_pending = self.internal_pending.saturating_sub(1);
                return Ok(());
            }
            ServerMessage::Result { outcome, .. } => {
                self.inflight = self.inflight.saturating_sub(1);
                if let Outcome::Ok(ok) = outcome
                    && let Some(revision) = ok.revision
                {
                    self.catch_up(revision).await?;
                }
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
                if shutdown {
                    // The control loop stops the show and ends (docs/network.md); the process
                    // exit is the host's.
                    let _ = self.shared.inbox.send(Inbox::Client {
                        message: ClientMessage::Goodbye { shutdown: true },
                        reply: self.reply_tx.clone(),
                    });
                }
                Err(Closed)
            }
            other => {
                if request_id(&other) == Some(INTERNAL_ID) {
                    return self
                        .send(&error_message(
                            ErrorCode::BadRequest,
                            "request id is reserved",
                        ))
                        .await;
                }
                self.inflight += 1;
                let sent = self.shared.inbox.send(Inbox::Client {
                    message: other,
                    reply: self.reply_tx.clone(),
                });
                if sent.is_err() {
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
