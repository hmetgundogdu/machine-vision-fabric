//! A supervised child process speaking the stdio protocol: bounded waits, enforceable termination,
//! exit classification and guaranteed reaping (no orphan in supported cases).

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mvf_engine::{CancellationToken, LogEvent, Logs, NodeError, NodeErrorKind, StopOutcome};

use crate::protocol::{ChildMessage, parse_line};

/// Upper bound on one stdout/stderr line; longer lines are discarded with a warning.
pub const MAX_LINE: usize = 1024 * 1024;
const POLL: Duration = Duration::from_millis(5);

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
}

enum Event {
    Message(ChildMessage),
    Eof,
}

/// A running worker child.
pub struct WorkerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    readers: Vec<JoinHandle<()>>,
    exit: Option<ExitInfo>,
    node: String,
    /// Generation stamped on the child's log lines; a warm spare is re-stamped when acquired.
    generation: Arc<AtomicU64>,
    logs: Logs,
}

impl std::fmt::Debug for WorkerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerProcess")
            .field("pid", &self.child.id())
            .field("node", &self.node)
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

impl WorkerProcess {
    /// Spawns the child with piped stdio and starts the reader threads.
    pub fn spawn(
        spec: &LaunchSpec,
        node: &str,
        generation: u64,
        logs: &Logs,
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
        let (tx, events) = mpsc::channel();
        let mut readers = Vec::new();

        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("child stdio was not piped"));
        };

        let out_logs = logs.clone();
        let out_node = node.to_string();
        let out_generation = Arc::clone(&shared_generation);
        readers.push(std::thread::spawn(move || {
            let log = |level: &str, message: String| {
                out_logs.push(LogEvent {
                    node: out_node.clone(),
                    generation: out_generation.load(Ordering::Relaxed),
                    level: level.into(),
                    message,
                })
            };
            read_lines(BufReader::new(stdout), |line| match line {
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
        }));

        let err_logs = logs.clone();
        let err_node = node.to_string();
        let err_generation = Arc::clone(&shared_generation);
        readers.push(std::thread::spawn(move || {
            read_lines(BufReader::new(stderr), |line| {
                let message = match line {
                    Ok(l) => l,
                    Err(n) => format!("discarded an overlong stderr line ({n} bytes)"),
                };
                err_logs.push(LogEvent {
                    node: err_node.clone(),
                    generation: err_generation.load(Ordering::Relaxed),
                    level: "stderr".into(),
                    message,
                });
            });
        }));

        Ok(Self {
            stdin: child.stdin.take(),
            child,
            events,
            readers,
            exit: None,
            node: node.to_string(),
            generation: shared_generation,
            logs: logs.clone(),
        })
    }

    /// Re-stamps the generation on this child's log lines (a warm spare taken by a new generation).
    pub fn set_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
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

    /// Writes one protocol line. A broken pipe means the child is gone.
    pub fn send(&mut self, line: &str) -> Result<(), WaitError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(WaitError::Exited(self.reap()));
        };
        let ok = stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush());
        if ok.is_err() {
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
            let mut wait = POLL.max(Duration::from_millis(1));
            if cancel.is_none() {
                wait = Duration::from_millis(100);
            }
            if let Some(d) = deadline {
                let now = Instant::now();
                if now >= d {
                    return Err(WaitError::Timeout);
                }
                wait = wait.min(d - now);
            }
            match self.events.recv_timeout(wait) {
                Ok(Event::Message(m)) if accept(&m) => return Ok(m),
                Ok(Event::Message(m)) => self.logs.push(LogEvent {
                    node: self.node.clone(),
                    generation: self.generation.load(Ordering::Relaxed),
                    level: "warn".into(),
                    message: format!("ignored unexpected message {m:?}"),
                }),
                Ok(Event::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(WaitError::Exited(self.reap()));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
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
        let _ = self.send(&crate::protocol::shutdown());
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
