// SPDX-License-Identifier: GPL-3.0-or-later

//! The stage host supervisor: starts `openlab-stage` (the process that owns the video output
//! windows), speaks the stage link to it, and keeps it alive. A crash or hang there never
//! reaches sound: the stage host is a child process, everything it sends is validated
//! (`stage_link`), and its pipes are read and written by threads of their own.
//!
//! - **Health.** The stage host must answer `hello` and every `ping` within
//!   [`StageHostConfig::answer_within`]; one that does not, that stops reading its input, or
//!   that floods its output is killed.
//! - **Relaunch.** A stage host that exits or is killed is started again at once and given the
//!   last stage set, so picture comes back by itself. More than
//!   [`StageHostConfig::max_restarts`] starts within [`StageHostConfig::restart_window`] and the
//!   supervisor waits [`StageHostConfig::retry_after`] before trying again (logged).
//! - **Never an orphan.** The stage host exits when its stdin ends, which happens when this
//!   process ends however it ends; [`StageHost`]'s drop also asks it to quit.
//!
//! The control thread only sends commands into a channel and gets [`StageEvent`]s back through
//! its inbox: it never waits on the stage host.

use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, never, select};
use openlab_core::StageId;
use openlab_core::stage_link::{
    Display, FromStage, LINK_VERSION, Line, LinkLevel, StageOutput, StageState, ToStage,
    decode_from_stage, encode, read_line,
};

/// How the engine runs its stage host.
#[derive(Clone, Debug)]
pub struct StageHostConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// How often a running stage host is pinged.
    pub ping_every: Duration,
    /// How long `hello` and each `ping` may wait for their answer.
    pub answer_within: Duration,
    /// Starts allowed within `restart_window` before the supervisor pauses.
    pub max_restarts: usize,
    pub restart_window: Duration,
    /// The pause after too many starts.
    pub retry_after: Duration,
}

impl StageHostConfig {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            args: Vec::new(),
            ping_every: Duration::from_secs(1),
            answer_within: Duration::from_secs(5),
            max_restarts: 5,
            restart_window: Duration::from_secs(60),
            retry_after: Duration::from_secs(30),
        }
    }
}

/// What the supervisor tells the control thread.
#[derive(Clone, Debug, PartialEq)]
pub enum StageEvent {
    /// A stage host answered `hello`. `video` is false where it has no video output.
    Started {
        pid: u32,
        video: bool,
    },
    Displays(Vec<Display>),
    Status {
        stage: StageId,
        state: StageState,
    },
    /// The stage host is gone: it exited, or was killed for `reason`. `restarting`: another is
    /// being started now; otherwise video output is off until the supervisor retries.
    Stopped {
        reason: String,
        restarting: bool,
    },
    /// For the show log.
    Log {
        level: LinkLevel,
        message: String,
    },
    /// Test support: a probe's answer.
    #[cfg(feature = "test-support")]
    Probed {
        stage: StageId,
        frame: Option<u16>,
        colours: Vec<[u8; 4]>,
    },
}

pub type EventSink = Arc<dyn Fn(StageEvent) + Send + Sync>;

/// The handle the control thread holds. Dropping it asks the stage host to quit.
pub struct StageHost {
    commands: Sender<Order>,
}

enum Order {
    Stages(Vec<StageOutput>),
    #[cfg(feature = "test-support")]
    Send(ToStage),
    Quit,
}

impl StageHost {
    /// Starts the supervisor thread, which starts the stage host.
    pub fn start(config: StageHostConfig, events: EventSink) -> std::io::Result<Self> {
        let (commands, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("openlab-stage-link".into())
            .spawn(move || Supervisor::new(config, rx, events).run())?;
        Ok(Self { commands })
    }

    /// Sets the stages the stage host shows (every open workspace's), replacing the last set.
    pub fn set_stages(&self, stages: Vec<StageOutput>) {
        let _ = self.commands.send(Order::Stages(stages));
    }

    /// Test support: sends `message` as it is.
    #[cfg(feature = "test-support")]
    pub fn send(&self, message: ToStage) {
        let _ = self.commands.send(Order::Send(message));
    }
}

impl Drop for StageHost {
    fn drop(&mut self) {
        let _ = self.commands.send(Order::Quit);
    }
}

/// Lines the writer thread may have queued for a stage host before it counts as not reading.
const WRITE_QUEUE: usize = 64;
/// Messages the reader may hold for the supervisor; it waits beyond that.
const READ_QUEUE: usize = 256;
/// More lines than this from a stage host within one second and it is killed as broken.
const MAX_LINES_PER_SECOND: u32 = 2_000;
/// Bad lines reported per stage host; later ones are only counted.
const MAX_BAD_LINES_LOGGED: u32 = 20;
/// `log` messages from a stage host put in the show log per second; later ones are dropped.
const MAX_LOGS_PER_SECOND: u32 = 20;
/// Lines of a stage host's stderr put in the log; later ones are dropped.
const MAX_STDERR_LINES: u32 = 50;
/// How long a quitting stage host gets before it is killed.
const QUIT_WAIT: Duration = Duration::from_secs(1);

enum FromChild {
    Message(FromStage),
    Bad(String),
    End,
}

struct Running {
    child: Child,
    to_child: Sender<String>,
    from_child: Receiver<FromChild>,
    started: Instant,
    hello: bool,
    ping: Option<(u64, Instant)>,
    next_ping: Instant,
    window: Instant,
    lines: u32,
    logs: u32,
    bad: u32,
}

struct Supervisor {
    config: StageHostConfig,
    commands: Receiver<Order>,
    events: EventSink,
    stages: Option<Vec<StageOutput>>,
    running: Option<Running>,
    starts: VecDeque<Instant>,
    paused_until: Option<Instant>,
    /// A stage host of another link version: never started again.
    incompatible: bool,
    seq: u64,
}

impl Supervisor {
    fn new(config: StageHostConfig, commands: Receiver<Order>, events: EventSink) -> Self {
        Self {
            config,
            commands,
            events,
            stages: None,
            running: None,
            starts: VecDeque::new(),
            paused_until: None,
            incompatible: false,
            seq: 0,
        }
    }

    fn run(mut self) {
        loop {
            let now = Instant::now();
            if self.running.is_none() && self.may_start(now) {
                self.start(now);
            }
            let from_child = self
                .running
                .as_ref()
                .map_or_else(never, |r| r.from_child.clone());
            let wait = self
                .next_deadline(now)
                .map_or(Duration::from_secs(3600), |d| {
                    d.saturating_duration_since(now)
                });
            select! {
                recv(self.commands) -> command => match command {
                    Ok(Order::Stages(stages)) => {
                        if self.running.as_ref().is_some_and(|r| r.hello) {
                            self.write(&ToStage::Stages { stages: stages.clone() });
                        }
                        self.stages = Some(stages);
                    }
                    #[cfg(feature = "test-support")]
                    Ok(Order::Send(message)) => self.write(&message),
                    Ok(Order::Quit) | Err(_) => {
                        self.quit();
                        return;
                    }
                },
                recv(from_child) -> line => match line {
                    Ok(line) => self.heard(line),
                    Err(_) => self.stop("its output closed".into()),
                },
                default(wait) => {}
            }
            self.check_health(Instant::now());
        }
    }

    fn may_start(&mut self, now: Instant) -> bool {
        if self.incompatible {
            return false;
        }
        match self.paused_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.paused_until = None;
                self.starts.clear();
                true
            }
            None => true,
        }
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        match &self.running {
            Some(r) if !r.hello => Some(r.started + self.config.answer_within),
            Some(r) => Some(match r.ping {
                Some((_, sent)) => sent + self.config.answer_within,
                None => r.next_ping,
            }),
            None => self.paused_until.or(Some(now + self.config.retry_after)),
        }
    }

    fn start(&mut self, now: Instant) {
        self.starts.push_back(now);
        while self
            .starts
            .front()
            .is_some_and(|t| now.duration_since(*t) > self.config.restart_window)
        {
            self.starts.pop_front();
        }
        match spawn(&self.config, &self.events) {
            Ok((child, to_child, from_child)) => {
                self.running = Some(Running {
                    child,
                    to_child,
                    from_child,
                    started: now,
                    hello: false,
                    ping: None,
                    next_ping: now + self.config.ping_every,
                    window: now,
                    lines: 0,
                    logs: 0,
                    bad: 0,
                });
                self.write(&ToStage::Hello {
                    version: LINK_VERSION,
                });
            }
            Err(e) => self.stopped(format!("could not be started ({e})")),
        }
    }

    fn write(&mut self, message: &ToStage) {
        let Some(r) = &self.running else { return };
        let Ok(line) = encode(message) else { return };
        match r.to_child.try_send(line) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => self.stop("it stopped reading its input".into()),
        }
    }

    fn heard(&mut self, line: FromChild) {
        let now = Instant::now();
        let Some(r) = self.running.as_mut() else {
            return;
        };
        if now.duration_since(r.window) >= Duration::from_secs(1) {
            r.window = now;
            r.lines = 0;
            r.logs = 0;
        }
        r.lines += 1;
        if r.lines > MAX_LINES_PER_SECOND {
            return self.stop("it sent more than it may".into());
        }
        let message = match line {
            FromChild::End => return self.stop("it exited".into()),
            FromChild::Bad(why) => {
                r.bad += 1;
                if r.bad <= MAX_BAD_LINES_LOGGED {
                    (self.events)(StageEvent::Log {
                        level: LinkLevel::Warn,
                        message: format!("stage host: ignored a line: {why}"),
                    });
                }
                return;
            }
            FromChild::Message(message) => message,
        };
        let event = match message {
            FromStage::Hello {
                version,
                pid,
                video,
            } => {
                if version != LINK_VERSION {
                    self.incompatible = true;
                    return self.stop(format!(
                        "it speaks stage link version {version}; this engine speaks {LINK_VERSION}"
                    ));
                }
                r.hello = true;
                (self.events)(StageEvent::Started { pid, video });
                if let Some(stages) = self.stages.clone() {
                    self.write(&ToStage::Stages { stages });
                }
                return;
            }
            FromStage::Pong { seq } => {
                if r.ping.is_some_and(|(s, _)| s == seq) {
                    r.ping = None;
                }
                return;
            }
            FromStage::Displays { displays } => StageEvent::Displays(displays),
            FromStage::StageStatus { stage, state } => StageEvent::Status { stage, state },
            FromStage::Log { .. } if r.logs >= MAX_LOGS_PER_SECOND => return,
            FromStage::Log { level, message } => {
                r.logs += 1;
                StageEvent::Log {
                    level,
                    message: format!("stage host: {message}"),
                }
            }
            #[cfg(feature = "test-support")]
            FromStage::Probed {
                stage,
                frame,
                colours,
            } => StageEvent::Probed {
                stage,
                frame,
                colours,
            },
        };
        (self.events)(event);
    }

    fn check_health(&mut self, now: Instant) {
        let Some(r) = self.running.as_mut() else {
            return;
        };
        if !r.hello {
            if now.duration_since(r.started) > self.config.answer_within {
                self.stop("it did not answer hello".into());
            }
            return;
        }
        match r.ping {
            Some((_, sent)) if now.duration_since(sent) > self.config.answer_within => {
                self.stop("it stopped answering".into());
            }
            Some(_) => {}
            None if now >= r.next_ping => {
                self.seq = self.seq.wrapping_add(1);
                r.ping = Some((self.seq, now));
                r.next_ping = now + self.config.ping_every;
                let seq = self.seq;
                self.write(&ToStage::Ping { seq });
            }
            None => {}
        }
    }

    /// Kills the running stage host and reports it.
    fn stop(&mut self, reason: String) {
        if let Some(r) = self.running.take() {
            reap(r.child, true);
        }
        self.stopped(reason);
    }

    fn stopped(&mut self, reason: String) {
        let restarting = !self.incompatible && self.starts.len() < self.config.max_restarts;
        if !restarting && !self.incompatible {
            self.paused_until = Some(Instant::now() + self.config.retry_after);
        }
        (self.events)(StageEvent::Stopped { reason, restarting });
    }

    /// Asks the stage host to quit, then kills what is left.
    fn quit(&mut self) {
        self.write(&ToStage::Quit);
        if let Some(r) = self.running.take() {
            // Closing its input ends it too, even if `quit` was not read.
            drop(r.to_child);
            reap(r.child, false);
        }
    }
}

/// Waits for `child` on a thread of its own (a process stuck in the kernel must not hold the
/// supervisor), killing it first or after [`QUIT_WAIT`].
fn reap(mut child: Child, kill_now: bool) {
    if kill_now {
        let _ = child.kill();
    }
    let reaper = std::thread::Builder::new()
        .name("openlab-stage-reap".into())
        .spawn(move || {
            let deadline = Instant::now() + QUIT_WAIT;
            while !kill_now && Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    // Without a thread the child is reaped when this process ends; it has already been told to
    // end (killed, or its input closed).
    drop(reaper);
}

type Spawned = (Child, Sender<String>, Receiver<FromChild>);

/// Starts the stage host with its stdin written by a writer thread, its stdout read by a reader
/// thread and its stderr forwarded (bounded) to the log.
fn spawn(config: &StageHostConfig, events: &EventSink) -> std::io::Result<Spawned> {
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group: a terminal's Ctrl-C reaches the engine, which decides.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn()?;
    let (stdin, stdout, stderr) =
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(i), Some(o), Some(e)) => (i, o, e),
            _ => {
                reap(child, true);
                return Err(std::io::Error::other("the stage host's pipes are missing"));
            }
        };
    let (to_child, lines_out) = bounded(WRITE_QUEUE);
    let (lines_in, from_child) = bounded(READ_QUEUE);
    let started = std::thread::Builder::new()
        .name("openlab-stage-write".into())
        .spawn(move || write_lines(stdin, &lines_out))
        .and_then(|_| {
            std::thread::Builder::new()
                .name("openlab-stage-read".into())
                .spawn(move || read_lines(stdout, &lines_in))
        })
        .and_then(|_| {
            let events = Arc::clone(events);
            std::thread::Builder::new()
                .name("openlab-stage-stderr".into())
                .spawn(move || forward_stderr(stderr, &events))
        });
    if let Err(e) = started {
        reap(child, true);
        return Err(e);
    }
    Ok((child, to_child, from_child))
}

fn write_lines(mut stdin: ChildStdin, lines: &Receiver<String>) {
    while let Ok(line) = lines.recv() {
        if stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return;
        }
    }
}

fn read_lines(stdout: impl std::io::Read, out: &Sender<FromChild>) {
    let mut input = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        let item = match read_line(&mut input, &mut line) {
            Ok(Line::End) | Err(_) => {
                let _ = out.send(FromChild::End);
                return;
            }
            Ok(Line::TooLong(bytes)) => FromChild::Bad(format!("a line of {bytes} bytes")),
            Ok(Line::Read) => match decode_from_stage(&line) {
                Ok(message) => FromChild::Message(message),
                Err(e) => FromChild::Bad(e.to_string()),
            },
        };
        if out.send(item).is_err() {
            return;
        }
    }
}

fn forward_stderr(stderr: impl std::io::Read, events: &EventSink) {
    let mut input = BufReader::new(stderr);
    let mut line = Vec::new();
    let mut sent = 0u32;
    loop {
        line.clear();
        match read_line(&mut input, &mut line) {
            Ok(Line::End) | Err(_) => return,
            Ok(Line::TooLong(_)) => {}
            Ok(Line::Read) if sent < MAX_STDERR_LINES => {
                sent += 1;
                let text = String::from_utf8_lossy(&line);
                let text: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
                events(StageEvent::Log {
                    level: LinkLevel::Warn,
                    message: format!("stage host: {text}"),
                });
            }
            Ok(Line::Read) => {}
        }
    }
}
