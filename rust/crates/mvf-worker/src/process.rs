//! A supervised child process speaking the worker protocol (v1 JSON lines or v2 binary frames over
//! its stdio): bounded waits, enforceable termination, exit classification and guaranteed reaping
//! (no orphan in supported cases).
//!
//! Receive paths, chosen by measurement (#32, `crates/mvf-ipc/tests/bench.rs`):
//! - **v2 on Unix**: the calling thread reads frames itself from a non-blocking pipe with `poll`,
//!   so a reply costs one context switch, and a frame left half-written is bounded by the request's
//!   deadline.
//! - **v1, and v2 on Windows** (anonymous pipes cannot be polled): a reader thread hands messages
//!   over a channel.
//!
//! Logs never share the control path: they go through a bounded queue to a delivery thread, so a
//! slow log consumer drops (and counts) lines instead of stalling execution.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mvf_engine::{CancellationToken, LogEvent, Logs, NodeError, NodeErrorKind, StopOutcome};

use crate::protocol::{ChildMessage, HostMessage, decode_frame, parse_line};

/// Upper bound on one stdout/stderr line; longer lines are discarded with a warning.
pub const MAX_LINE: usize = 1024 * 1024;
const POLL: Duration = Duration::from_millis(5);
/// Log lines queued for delivery before new ones are dropped (and counted).
pub const LOG_QUEUE: usize = 4096;

/// What to launch.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Program.
    pub program: OsString,
    /// Arguments.
    pub args: Vec<OsString>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Extra environment.
    pub env: Vec<(String, OsString)>,
    /// Inherited variables the child must not see (e.g. the display server of a node without a
    /// native display grant).
    pub remove_env: Vec<String>,
}

/// The engine's v2 offer for one launch: the child proves it is the process this launch started
/// by echoing `session` in its `hello` (launch binding; the channel itself is an inherited pipe
/// with no endpoint anyone could connect to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// Per-launch token.
    pub session: String,
    /// Generation the child starts in.
    pub generation: u64,
}

impl Offer {
    /// A fresh offer.
    pub fn new(generation: u64) -> Self {
        use std::hash::{BuildHasher, Hasher};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let a = h.finish();
        h.write_u64(a);
        Self {
            session: format!("{a:016x}{:016x}", h.finish()),
            generation,
        }
    }

    /// The environment that carries the offer.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("MVF_PROTOCOL".into(), "2".into()),
            ("MVF_SESSION".into(), self.session.clone()),
            ("MVF_GENERATION".into(), self.generation.to_string()),
        ]
    }
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    /// Exit code, when it exited normally.
    pub code: Option<i32>,
    /// Terminating signal (Unix).
    pub signal: Option<i32>,
}

impl ExitInfo {
    fn from_status(status: ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: status.code(),
            signal,
        }
    }

    /// A `ProcessExited` node error describing this exit.
    pub fn to_error(self, context: &str) -> NodeError {
        let how = match (self.code, self.signal) {
            (_, Some(sig)) => format!("was terminated by signal {sig}{}", signal_name(sig)),
            (Some(code), None) => format!("exited with code {code}"),
            (None, None) => "exited".to_string(),
        };
        let mut e = NodeError::new(
            NodeErrorKind::ProcessExited,
            format!("worker {how} {context}"),
        );
        e.exit_code = self.code;
        e.signal = self.signal;
        e
    }
}

fn signal_name(sig: i32) -> &'static str {
    match sig {
        6 => " (SIGABRT)",
        9 => " (SIGKILL)",
        11 => " (SIGSEGV)",
        15 => " (SIGTERM)",
        _ => "",
    }
}

/// Why a wait ended without the expected message.
#[derive(Debug)]
pub enum WaitError {
    /// The deadline elapsed.
    Timeout,
    /// The cancellation token was set.
    Cancelled,
    /// The child exited (stdout closed).
    Exited(ExitInfo),
    /// The child broke the protocol (malformed, oversized or stale frame, wrong session): it is
    /// not trusted to resynchronise and is terminated.
    Protocol(String),
}

enum Event {
    Message(ChildMessage),
    /// The detector found the child's protocol version.
    Version(u64),
    Violation(String),
    Eof,
}

enum Rx {
    /// stdout not classified yet (v2 offered on Unix: the first byte decides).
    #[cfg(unix)]
    Pending(Option<ChildStdout>),
    /// A reader thread delivers messages.
    Thread(Receiver<Event>),
    /// The calling thread reads v2 frames itself.
    #[cfg(unix)]
    Direct(direct::Reader),
}

/// Bounded, non-blocking log delivery.
#[derive(Clone)]
struct LogSink {
    tx: SyncSender<LogEvent>,
    dropped: Arc<AtomicU64>,
}

impl LogSink {
    fn new(logs: &Logs) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::sync_channel::<LogEvent>(LOG_QUEUE);
        let logs = logs.clone();
        let handle = std::thread::spawn(move || {
            for e in rx {
                logs.push(e);
            }
        });
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            handle,
        )
    }

    fn push(&self, e: LogEvent) {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            let summary = LogEvent {
                level: "warn".into(),
                message: format!("dropped {dropped} log line(s): the log consumer is too slow"),
                ..e.clone()
            };
            if self.tx.try_send(summary).is_err() {
                self.dropped.fetch_add(dropped, Ordering::Relaxed);
            }
        }
        if let Err(TrySendError::Full(_)) = self.tx.try_send(e) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn finish(&self, node: &str, generation: u64) {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            // The process is gone: wait for room rather than lose the count.
            let _ = self.tx.send(LogEvent {
                node: node.to_string(),
                generation,
                level: "warn".into(),
                message: format!("dropped {dropped} log line(s): the log consumer is too slow"),
            });
        }
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A running worker child.
pub struct WorkerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Rx,
    readers: Vec<JoinHandle<()>>,
    exit: Option<ExitInfo>,
    node: String,
    /// Generation stamped on the child's log lines and v2 frames; a warm spare is re-stamped when
    /// acquired.
    generation: Arc<AtomicU64>,
    logs: Logs,
    sink: LogSink,
    /// Read by the Unix direct path (on Windows the detecting reader thread owns a copy).
    #[cfg_attr(not(unix), allow(dead_code))]
    offer: Option<Offer>,
    /// 0 until known, then 1 or 2.
    protocol: u64,
}

impl std::fmt::Debug for WorkerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerProcess")
            .field("pid", &self.child.id())
            .field("node", &self.node)
            .field("protocol", &self.protocol)
            .field("exit", &self.exit)
            .finish()
    }
}

fn read_lines(mut reader: impl BufRead, mut on_line: impl FnMut(Result<String, usize>)) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE && buf.last() != Some(&b'\n') {
            // Discard the rest of an overlong line without buffering it.
            let mut discarded = buf.len();
            loop {
                buf.clear();
                match (&mut reader)
                    .take(MAX_LINE as u64)
                    .read_until(b'\n', &mut buf)
                {
                    Ok(0) | Err(_) => return,
                    Ok(n) => discarded += n,
                }
                if buf.last() == Some(&b'\n') {
                    break;
                }
            }
            on_line(Err(discarded));
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        on_line(Ok(line.trim_end_matches(['\r', '\n']).to_string()));
    }
}

/// A stderr line as a log event: a v2 SDK writes `{"level": …, "message": …}`, anything else is raw
/// `stderr`.
fn stderr_event(line: String) -> (String, String) {
    if line.starts_with('{')
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
        && let (Some(level), Some(message)) = (
            v.get("level").and_then(|l| l.as_str()),
            v.get("message").and_then(|m| m.as_str()),
        )
    {
        return (level.to_string(), message.to_string());
    }
    ("stderr".into(), line)
}

/// The session check of a v2 `hello`.
fn check_session(offer: Option<&Offer>, body: &[u8]) -> Result<(), String> {
    let Some(offer) = offer else {
        return Err("a v2 hello without a v2 offer".into());
    };
    let session = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("session")
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if session != offer.session {
        return Err(
            "unauthorized peer: the hello does not carry this launch's session token".into(),
        );
    }
    Ok(())
}

/// Reads v1 lines (detector already consumed `prefix`).
fn v1_reader(
    reader: impl Read + Send + 'static,
    tx: mpsc::Sender<Event>,
    sink: LogSink,
    node: String,
    generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let log = |level: &str, message: String| {
            sink.push(LogEvent {
                node: node.clone(),
                generation: generation.load(Ordering::Relaxed),
                level: level.into(),
                message,
            })
        };
        let _ = tx.send(Event::Version(1));
        read_lines(BufReader::new(reader), |line| match line {
            Err(n) => log(
                "warn",
                format!("discarded an overlong stdout line ({n} bytes)"),
            ),
            Ok(line) if line.trim().is_empty() => {}
            Ok(line) => match parse_line(&line) {
                Ok(ChildMessage::Log { level, message }) => log(&level, message),
                Ok(message) => {
                    let _ = tx.send(Event::Message(message));
                }
                Err(raw) => log("warn", format!("non-protocol stdout: {raw}")),
            },
        });
        let _ = tx.send(Event::Eof);
    })
}

/// Reads v2 frames on a thread (Windows, or wherever the direct path is unavailable).
#[cfg(not(unix))]
fn v2_reader(
    mut reader: impl Read + Send + 'static,
    tx: mpsc::Sender<Event>,
    offer: Option<Offer>,
    generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = tx.send(Event::Version(2));
        let mut body = Vec::new();
        loop {
            let h = match mvf_ipc::read_frame(&mut reader, mvf_ipc::MAX_FRAME, &mut body) {
                Ok(h) => h,
                Err(mvf_ipc::FrameError::Eof) => break,
                Err(mvf_ipc::FrameError::Truncated { .. }) | Err(mvf_ipc::FrameError::Io(_)) => {
                    break;
                }
                Err(e) => {
                    let _ = tx.send(Event::Violation(e.to_string()));
                    return;
                }
            };
            let event = classify(
                &h,
                &body,
                offer.as_ref(),
                generation.load(Ordering::Relaxed),
            );
            let stop = matches!(event, Event::Violation(_));
            let _ = tx.send(event);
            if stop {
                return;
            }
        }
        let _ = tx.send(Event::Eof);
    })
}

/// One received v2 frame as an event (violations included).
fn classify(h: &mvf_ipc::Header, body: &[u8], offer: Option<&Offer>, generation: u64) -> Event {
    if h.kind == mvf_ipc::Kind::Hello {
        if let Err(e) = check_session(offer, body) {
            return Event::Violation(e);
        }
    } else if u64::from(h.generation) != generation {
        return Event::Violation(format!(
            "stale generation: a {:?} frame for generation {} reached generation {generation}",
            h.kind, h.generation
        ));
    }
    match decode_frame(h, body) {
        Ok(m) => Event::Message(m),
        Err(e) => Event::Violation(e),
    }
}

/// Detects the child's protocol from its first stdout byte on a thread (Windows), then reads on.
#[cfg(not(unix))]
fn detecting_reader(
    mut stdout: ChildStdout,
    tx: mpsc::Sender<Event>,
    sink: LogSink,
    node: String,
    offer: Option<Offer>,
    generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut first = [0u8; 1];
        match stdout.read(&mut first) {
            Ok(1) => {}
            _ => {
                let _ = tx.send(Event::Eof);
                return;
            }
        }
        let chained = std::io::Cursor::new(first.to_vec()).chain(stdout);
        let inner = if first[0] == mvf_ipc::MAGIC[0] {
            v2_reader(chained, tx, offer, generation)
        } else {
            v1_reader(chained, tx, sink, node, generation)
        };
        let _ = inner.join();
    })
}

impl WorkerProcess {
    /// Spawns the child with piped stdio and starts the reader threads. With an `offer` (whose
    /// environment the launcher already put into `spec`) the child may answer in v2.
    pub fn spawn(
        spec: &LaunchSpec,
        node: &str,
        generation: u64,
        logs: &Logs,
        offer: Option<Offer>,
    ) -> std::io::Result<Self> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for k in &spec.remove_env {
            command.env_remove(k);
        }
        for (k, v) in &spec.env {
            command.env(k, v);
        }
        let mut child = crate::spawner::spawn(command)?;
        let shared_generation = Arc::new(AtomicU64::new(generation));
        let mut readers = Vec::new();

        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("child stdio was not piped"));
        };
        let (sink, delivery) = LogSink::new(logs);
        readers.push(delivery);

        let rx = match &offer {
            None => {
                let (tx, rx) = mpsc::channel();
                readers.push(v1_reader(
                    stdout,
                    tx,
                    sink.clone(),
                    node.to_string(),
                    Arc::clone(&shared_generation),
                ));
                Rx::Thread(rx)
            }
            #[cfg(unix)]
            Some(_) => Rx::Pending(Some(stdout)),
            #[cfg(not(unix))]
            Some(o) => {
                let (tx, rx) = mpsc::channel();
                readers.push(detecting_reader(
                    stdout,
                    tx,
                    sink.clone(),
                    node.to_string(),
                    Some(o.clone()),
                    Arc::clone(&shared_generation),
                ));
                Rx::Thread(rx)
            }
        };

        let err_sink = sink.clone();
        let err_node = node.to_string();
        let err_generation = Arc::clone(&shared_generation);
        readers.push(std::thread::spawn(move || {
            read_lines(BufReader::new(stderr), |line| {
                let (level, message) = match line {
                    Ok(l) => stderr_event(l),
                    Err(n) => (
                        "stderr".into(),
                        format!("discarded an overlong stderr line ({n} bytes)"),
                    ),
                };
                err_sink.push(LogEvent {
                    node: err_node.clone(),
                    generation: err_generation.load(Ordering::Relaxed),
                    level,
                    message,
                });
            });
            err_sink.finish(&err_node, err_generation.load(Ordering::Relaxed));
        }));

        Ok(Self {
            stdin: child.stdin.take(),
            child,
            rx,
            readers,
            exit: None,
            node: node.to_string(),
            generation: shared_generation,
            logs: logs.clone(),
            sink,
            offer,
            protocol: 0,
        })
    }

    /// Re-stamps the generation on this child's log lines and frames (a warm spare taken by a
    /// new generation).
    pub fn set_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
    }

    /// The protocol the child speaks (0 before its `hello`).
    pub fn protocol(&self) -> u64 {
        self.protocol
    }

    /// Log lines dropped so far because the consumer was too slow.
    pub fn dropped_logs(&self) -> u64 {
        self.sink.dropped()
    }

    /// True once the child has exited (polls without blocking).
    pub fn has_exited(&mut self) -> bool {
        self.exit.is_some() || matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// OS process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// How the child ended, if it has.
    pub fn exit(&mut self) -> Option<ExitInfo> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(ExitInfo::from_status(status));
        }
        self.exit
    }

    /// Sends one message in the child's protocol. A broken pipe means the child is gone.
    /// v2-only messages (`ping`, `cancel`) are no-ops for a v1 child.
    pub fn send(&mut self, message: &HostMessage) -> Result<(), WaitError> {
        let bytes = if self.protocol == 2 {
            message.to_frame(self.generation.load(Ordering::Relaxed) as u32)
        } else {
            match message.to_line() {
                Some(mut l) => {
                    l.push('\n');
                    l.into_bytes()
                }
                None => return Ok(()),
            }
        };
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(WaitError::Exited(self.reap()));
        };
        if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
            return Err(WaitError::Exited(self.reap()));
        }
        Ok(())
    }

    /// Waits for a message accepted by `accept`, bounded by `deadline` and `cancel`. Messages that
    /// are not accepted are logged and dropped.
    pub fn wait_for(
        &mut self,
        deadline: Option<Instant>,
        cancel: Option<&CancellationToken>,
        mut accept: impl FnMut(&ChildMessage) -> bool,
    ) -> Result<ChildMessage, WaitError> {
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err(WaitError::Cancelled);
            }
            let mut wait = if cancel.is_some() {
                POLL
            } else {
                Duration::from_millis(100)
            };
            if let Some(d) = deadline {
                let now = Instant::now();
                if now >= d {
                    return Err(WaitError::Timeout);
                }
                wait = wait.min(d - now);
            }
            let event = match &mut self.rx {
                #[cfg(unix)]
                Rx::Pending(stdout) => match direct::detect(stdout, wait) {
                    direct::Detected::NotYet => continue,
                    direct::Detected::Eof => Event::Eof,
                    direct::Detected::V2(reader) => {
                        self.rx = Rx::Direct(reader);
                        self.protocol = 2;
                        continue;
                    }
                    direct::Detected::V1(rest) => {
                        let (tx, rx) = mpsc::channel();
                        self.readers.push(v1_reader(
                            rest,
                            tx,
                            self.sink.clone(),
                            self.node.clone(),
                            Arc::clone(&self.generation),
                        ));
                        self.rx = Rx::Thread(rx);
                        continue;
                    }
                },
                #[cfg(unix)]
                Rx::Direct(reader) => match reader.next(wait) {
                    Ok(Some((h, body))) => classify(
                        &h,
                        &body,
                        self.offer.as_ref(),
                        self.generation.load(Ordering::Relaxed),
                    ),
                    Ok(None) => continue,
                    Err(direct::ReadError::Eof { inside_frame }) => {
                        if inside_frame {
                            self.log_warn("the channel closed inside a frame");
                        }
                        Event::Eof
                    }
                    Err(direct::ReadError::Violation(v)) => Event::Violation(v),
                },
                Rx::Thread(rx) => match rx.recv_timeout(wait) {
                    Ok(e) => e,
                    Err(RecvTimeoutError::Disconnected) => Event::Eof,
                    Err(RecvTimeoutError::Timeout) => continue,
                },
            };
            match event {
                Event::Version(v) => self.protocol = v,
                Event::Message(m) if accept(&m) => {
                    if let ChildMessage::Hello { protocol, .. } = &m
                        && self.protocol == 0
                    {
                        self.protocol = *protocol;
                    }
                    return Ok(m);
                }
                Event::Message(m) => self.log_warn(&format!("ignored unexpected message {m:?}")),
                Event::Violation(v) => {
                    self.kill();
                    return Err(WaitError::Protocol(v));
                }
                Event::Eof => return Err(WaitError::Exited(self.reap())),
            }
        }
    }

    fn log_warn(&self, message: &str) {
        self.logs.push(LogEvent {
            node: self.node.clone(),
            generation: self.generation.load(Ordering::Relaxed),
            level: "warn".into(),
            message: message.to_string(),
        });
    }

    /// Forcibly terminates the child and reaps it.
    pub fn kill(&mut self) -> ExitInfo {
        if let Some(exit) = self.exit() {
            return exit;
        }
        let _ = self.child.kill();
        self.reap()
    }

    /// Waits for the child to exit (blocking) and records the exit.
    fn reap(&mut self) -> ExitInfo {
        if let Some(exit) = self.exit {
            return exit;
        }
        self.stdin.take();
        let info = match self.child.wait() {
            Ok(status) => ExitInfo::from_status(status),
            Err(_) => ExitInfo {
                code: None,
                signal: None,
            },
        };
        self.exit = Some(info);
        info
    }

    /// Graceful stop: `shutdown`, close stdin, wait up to `grace`, then kill.
    pub fn shutdown(&mut self, grace: Duration) -> StopOutcome {
        if self.exit().is_some() {
            self.join_readers();
            return StopOutcome::AlreadyStopped;
        }
        if self.protocol != 0 {
            let _ = self.send(&HostMessage::Shutdown);
        }
        self.stdin.take();
        let until = Instant::now() + grace;
        let outcome = loop {
            if self.exit().is_some() {
                break StopOutcome::Graceful;
            }
            if Instant::now() >= until {
                self.kill();
                break StopOutcome::Forced;
            }
            std::thread::sleep(POLL);
        };
        self.join_readers();
        outcome
    }

    /// Joins reader threads that already saw EOF. A reader still blocked (a grandchild kept the pipe
    /// open) is detached rather than joined, so stopping never hangs; it exits when the pipe closes.
    fn join_readers(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.readers.iter().any(|r| !r.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        for r in self.readers.drain(..) {
            if r.is_finished() {
                let _ = r.join();
            }
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        // Never leave an orphan: whatever happened, terminate and reap.
        self.kill();
        self.join_readers();
    }
}

/// The Unix direct path: a non-blocking pipe, `poll`, and an incremental frame buffer.
#[cfg(unix)]
mod direct {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::process::ChildStdout;
    use std::time::Duration;

    use mvf_ipc::frame::{HEADER, Header};

    pub(super) enum Detected {
        NotYet,
        Eof,
        V2(Reader),
        V1(Box<dyn Read + Send>),
    }

    pub(super) enum ReadError {
        Eof { inside_frame: bool },
        Violation(String),
    }

    pub(super) struct Reader {
        stdout: ChildStdout,
        buf: Vec<u8>,
    }

    fn set_nonblocking(fd: i32, on: bool) {
        // SAFETY: plain fcntl on a descriptor we own.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            let flags = if on {
                flags | libc::O_NONBLOCK
            } else {
                flags & !libc::O_NONBLOCK
            };
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
    }

    /// Waits up to `wait` for the fd to become readable.
    fn readable(fd: i32, wait: Duration) -> bool {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = wait.as_millis().clamp(0, i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        r > 0
    }

    /// Reads what is available. `Ok(0)` = EOF.
    fn fill(stdout: &mut ChildStdout, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        let mut chunk = [0u8; 64 * 1024];
        match stdout.read(&mut chunk) {
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                Ok(n)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(e),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(e),
            Err(e) => Err(e),
        }
    }

    pub(super) fn detect(slot: &mut Option<ChildStdout>, wait: Duration) -> Detected {
        let Some(stdout) = slot.as_mut() else {
            return Detected::Eof;
        };
        let fd = stdout.as_raw_fd();
        set_nonblocking(fd, true);
        if !readable(fd, wait) {
            return Detected::NotYet;
        }
        let mut buf = Vec::new();
        match fill(stdout, &mut buf) {
            Ok(0) => return Detected::Eof,
            Ok(_) => {}
            Err(_) => return Detected::NotYet,
        }
        let Some(stdout) = slot.take() else {
            return Detected::Eof;
        };
        if buf[0] == mvf_ipc::MAGIC[0] {
            Detected::V2(Reader { stdout, buf })
        } else {
            set_nonblocking(fd, false);
            Detected::V1(Box::new(std::io::Cursor::new(buf).chain(stdout)))
        }
    }

    impl Reader {
        /// The next complete frame, or `None` if none arrived within `wait`.
        pub(super) fn next(
            &mut self,
            wait: Duration,
        ) -> Result<Option<(Header, Vec<u8>)>, ReadError> {
            loop {
                if let Some(frame) = self.take()? {
                    return Ok(Some(frame));
                }
                if !readable(self.stdout.as_raw_fd(), wait) {
                    return Ok(None);
                }
                match fill(&mut self.stdout, &mut self.buf) {
                    Ok(0) => {
                        return Err(ReadError::Eof {
                            inside_frame: !self.buf.is_empty(),
                        });
                    }
                    Ok(_) => {}
                    Err(_) => return Ok(None),
                }
            }
        }

        fn take(&mut self) -> Result<Option<(Header, Vec<u8>)>, ReadError> {
            if self.buf.len() < HEADER {
                return Ok(None);
            }
            let mut h = [0u8; HEADER];
            h.copy_from_slice(&self.buf[..HEADER]);
            let header = Header::decode(&h, mvf_ipc::MAX_FRAME)
                .map_err(|e| ReadError::Violation(e.to_string()))?;
            let total = HEADER + header.length as usize;
            if self.buf.len() < total {
                return Ok(None);
            }
            let body = self.buf[HEADER..total].to_vec();
            self.buf.drain(..total);
            Ok(Some((header, body)))
        }
    }
}
