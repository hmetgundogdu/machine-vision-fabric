//! The local management server: one accept thread and one thread per client, all outside the
//! engine. API version 1 is newline-delimited JSON (`protocol/management-v1.md`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use crate::command::{Audit, Commands, Control, Role, Session};
use crate::endpoint::{self, Instance};
use crate::feed::{Feed, Read};
use crate::model::API;
use crate::sys::{Conn, Listener};

/// Server limits.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Runtime directory (default: [`endpoint::runtime_dir`]).
    pub dir: Option<PathBuf>,
    /// Concurrent clients (default 8); more are told `busy` and closed.
    pub max_clients: usize,
    /// A write that blocks this long drops the client (default 2 s; Unix only).
    pub write_timeout: Duration,
    /// Longest request line (default 64 KiB).
    pub max_request: usize,
    /// An idle subscription gets a heartbeat this often (default 1 s).
    pub heartbeat: Duration,
    /// The run's controls; without them the endpoint is observation-only.
    pub control: Option<Control>,
    /// Append-only audit log of commands and refused connections.
    pub audit: Option<PathBuf>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            dir: None,
            max_clients: 8,
            write_timeout: Duration::from_secs(2),
            max_request: 64 * 1024,
            heartbeat: Duration::from_secs(1),
            control: None,
            audit: None,
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    clients: AtomicUsize,
    accepted: AtomicU64,
    refused: AtomicU64,
    disconnected: AtomicU64,
    /// The cursor of every streaming client (for a clean shutdown).
    cursors: std::sync::Mutex<Vec<Arc<AtomicU64>>>,
    /// Commands being carried out: their replies must go out before the endpoint closes (a
    /// `stop` ends the run, and the run's end closes the endpoint).
    commands_in_flight: AtomicUsize,
}

/// A running management endpoint. Dropping it closes the endpoint and its registration.
pub struct Server {
    feed: Arc<Feed>,
    stop: Arc<AtomicBool>,
    endpoint: String,
    registry: PathBuf,
    listener: Arc<std::sync::Mutex<Option<Listener>>>,
    thread: Option<JoinHandle<()>>,
    counters: Arc<Counters>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

impl Server {
    /// Opens the endpoint for `feed` and registers it.
    pub fn start(feed: Arc<Feed>, options: ServerOptions) -> std::io::Result<Self> {
        let dir = match &options.dir {
            Some(d) => {
                endpoint::ensure_private(d)?;
                d.clone()
            }
            None => endpoint::runtime_dir()?,
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "mvf-{}-{}-{nonce:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let (listener, endpoint) = Listener::bind(&dir, &name)?;
        let hello = feed.hello();
        let registry = endpoint::register(
            &dir,
            &name,
            &Instance {
                endpoint: endpoint.clone(),
                pid: std::process::id(),
                pipeline: hello["pipeline"].as_str().unwrap_or_default().to_string(),
                started_at_ms: hello["startedAtMs"].as_u64().unwrap_or(0),
                api: API,
                registry: PathBuf::new(),
            },
        )?;
        let audit = options
            .audit
            .as_deref()
            .map(Audit::open)
            .transpose()?
            .map(Arc::new);
        let commands = Arc::new(Commands::new(options.control.clone(), audit));
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let listener = Arc::new(std::sync::Mutex::new(Some(listener)));
        let thread = {
            let (stop, counters, listener, feed) = (
                stop.clone(),
                counters.clone(),
                listener.clone(),
                feed.clone(),
            );
            std::thread::Builder::new()
                .name("mvf-management".into())
                .spawn(move || {
                    accept_loop(&listener, &stop, &feed, &options, &counters, &commands)
                })?
        };
        Ok(Self {
            feed,
            stop,
            endpoint,
            registry,
            listener,
            thread: Some(thread),
            counters,
        })
    }

    /// The endpoint clients connect to.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Connected clients.
    pub fn clients(&self) -> usize {
        self.counters.clients.load(Ordering::Relaxed)
    }

    /// (accepted, refused, disconnected for a stuck or failed write).
    pub fn counters(&self) -> (u64, u64, u64) {
        (
            self.counters.accepted.load(Ordering::Relaxed),
            self.counters.refused.load(Ordering::Relaxed),
            self.counters.disconnected.load(Ordering::Relaxed),
        )
    }
}

impl Server {
    /// Waits up to `timeout` for every command in flight to have been answered and every
    /// streaming client to have been sent everything published so far (the final status, at the
    /// end of a run). A stuck client is not waited for longer.
    pub fn drain(&self, timeout: Duration) {
        let target = self.feed.last_seq();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let behind = self
                .counters
                .cursors
                .lock()
                .map(|c| c.iter().any(|c| c.load(Ordering::Relaxed) < target))
                .unwrap_or(false);
            let answering = self.counters.commands_in_flight.load(Ordering::SeqCst) > 0;
            if !behind && !answering {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.drain(Duration::from_secs(1));
        self.stop.store(true, Ordering::SeqCst);
        // The accept thread holds the listener only while blocked in accept; wake it.
        let waker = crate::sys::connect(&self.endpoint);
        drop(waker);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Ok(mut l) = self.listener.lock()
            && let Some(l) = l.take()
        {
            l.cleanup();
        }
        let _ = std::fs::remove_file(&self.registry);
    }
}

fn accept_loop(
    listener: &std::sync::Mutex<Option<Listener>>,
    stop: &Arc<AtomicBool>,
    feed: &Arc<Feed>,
    options: &ServerOptions,
    counters: &Arc<Counters>,
    commands: &Arc<Commands>,
) {
    loop {
        let accepted = {
            let Ok(mut guard) = listener.lock() else {
                return;
            };
            let Some(l) = guard.as_mut() else {
                return;
            };
            l.accept()
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok((mut conn, authorized)) = accepted else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        if !authorized {
            counters.refused.fetch_add(1, Ordering::Relaxed);
            if let Some(a) = commands.audit() {
                a.record(json!({"event": "connection-refused", "reason": "unauthorized", "via": "local"}));
            }
            let _ = send(
                &mut conn,
                &json!({"op": "error", "code": "unauthorized", "message": "this endpoint serves its owner only"}),
            );
            continue;
        }
        if counters.clients.load(Ordering::Relaxed) >= options.max_clients {
            counters.refused.fetch_add(1, Ordering::Relaxed);
            let _ = send(
                &mut conn,
                &json!({"op": "error", "code": "busy", "message": format!("at most {} clients", options.max_clients)}),
            );
            continue;
        }
        counters.accepted.fetch_add(1, Ordering::Relaxed);
        counters.clients.fetch_add(1, Ordering::Relaxed);
        let (feed, options, mine, stop, commands) = (
            feed.clone(),
            options.clone(),
            counters.clone(),
            stop.clone(),
            commands.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("mvf-management-client".into())
            .spawn(move || {
                // A bug in a client handler must never reach the engine or other clients.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve(conn, &feed, &options, &stop, &mine, &commands)
                }));
                if !matches!(result, Ok(Ok(()))) {
                    mine.disconnected.fetch_add(1, Ordering::Relaxed);
                }
                mine.clients.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            counters.clients.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn send(conn: &mut Box<dyn Conn>, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    conn.write_all(line.as_bytes())?;
    conn.flush()
}

/// Reads one request line of at most `max` bytes. `Ok(None)` at end of stream.
fn read_request(
    reader: &mut BufReader<Box<dyn Conn>>,
    max: usize,
) -> std::io::Result<Option<Result<String, usize>>> {
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(if line.is_empty() {
                None
            } else {
                Some(Err(line.len()))
            });
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..=i], true),
            None => (available, false),
        };
        let n = chunk.len();
        if line.len() + n > max {
            return Ok(Some(Err(line.len() + n)));
        }
        line.extend_from_slice(chunk);
        reader.consume(n);
        if done {
            return Ok(Some(Ok(String::from_utf8_lossy(&line).trim().to_string())));
        }
    }
}

fn error(code: &str, message: impl Into<String>) -> Value {
    json!({"op": "error", "code": code, "message": message.into()})
}

fn serve(
    conn: Box<dyn Conn>,
    feed: &Feed,
    options: &ServerOptions,
    stop: &AtomicBool,
    counters: &Counters,
    commands: &Commands,
) -> std::io::Result<()> {
    conn.set_write_timeout(options.write_timeout)?;
    let mut writer = conn.duplicate()?;
    let mut reader = BufReader::new(conn);
    let mut session: Option<Session> = None;
    loop {
        let line = match read_request(&mut reader, options.max_request)? {
            None => return Ok(()),
            Some(Err(n)) => {
                let _ = send(
                    &mut writer,
                    &error(
                        "too-large",
                        format!(
                            "a request is at most {} bytes, got {n}",
                            options.max_request
                        ),
                    ),
                );
                return Ok(());
            }
            Some(Ok(l)) if l.is_empty() => continue,
            Some(Ok(l)) => l,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(
                &mut writer,
                &error("malformed", "a request is one JSON object per line"),
            )?;
            return Ok(());
        };
        let op = msg["op"].as_str().unwrap_or_default();
        if session.is_none() && op != "hello" {
            send(&mut writer, &error("hello-first", "send hello first"))?;
            return Ok(());
        }
        match op {
            "hello" => {
                let offered: Vec<u64> = match &msg["api"] {
                    Value::Array(a) => a.iter().filter_map(Value::as_u64).collect(),
                    v => v.as_u64().into_iter().collect(),
                };
                if !offered.contains(&API) {
                    send(
                        &mut writer,
                        &error(
                            "api-unsupported",
                            format!(
                                "this runtime speaks API {API}, the client offered {offered:?}"
                            ),
                        ),
                    )?;
                    return Ok(());
                }
                let s = match Session::from_hello(&msg) {
                    Ok(s) => s,
                    Err(e) => {
                        send(&mut writer, &error("invalid", e))?;
                        return Ok(());
                    }
                };
                let features: Vec<&str> = (commands.available() && s.role == Role::Operator)
                    .then_some("commands")
                    .into_iter()
                    .collect();
                send(
                    &mut writer,
                    &json!({"op": "hello", "api": API, "runtime": feed.hello(), "principal": s.principal, "role": s.role.as_str(), "features": features}),
                )?;
                session = Some(s);
            }
            "ping" => send(&mut writer, &json!({"op": "pong"}))?,
            "command" => {
                let s = session.clone().unwrap_or_else(Session::local);
                counters.commands_in_flight.fetch_add(1, Ordering::SeqCst);
                let reply = commands.execute(feed, &s, &msg);
                let sent = send(&mut writer, &reply);
                counters.commands_in_flight.fetch_sub(1, Ordering::SeqCst);
                sent?;
            }
            "snapshot" => {
                let (seq, snapshot) = feed.snapshot();
                send(
                    &mut writer,
                    &json!({"op": "snapshot", "seq": seq, "snapshot": snapshot}),
                )?;
            }
            "subscribe" => {
                let from = msg["from"].as_u64();
                let cursor = Arc::new(AtomicU64::new(0));
                if let Ok(mut c) = counters.cursors.lock() {
                    c.push(cursor.clone());
                }
                let result = stream(writer, feed, from, options, stop, &cursor);
                if let Ok(mut c) = counters.cursors.lock() {
                    c.retain(|x| !Arc::ptr_eq(x, &cursor));
                }
                return result;
            }
            other => send(
                &mut writer,
                &error("unknown-op", format!("unknown op {other:?}")),
            )?,
        }
    }
}

/// Streams entries from `from` (or, without one, a snapshot and then everything after it).
fn stream(
    mut w: Box<dyn Conn>,
    feed: &Feed,
    from: Option<u64>,
    options: &ServerOptions,
    stop: &AtomicBool,
    sent: &AtomicU64,
) -> std::io::Result<()> {
    let mut cursor = match from {
        Some(f) => f.saturating_sub(1),
        None => {
            let (seq, snapshot) = feed.snapshot();
            send(
                &mut w,
                &json!({"op": "snapshot", "seq": seq, "snapshot": snapshot}),
            )?;
            seq
        }
    };
    let mut out = Vec::with_capacity(64 * 1024);
    while !stop.load(Ordering::SeqCst) {
        sent.store(cursor, Ordering::Relaxed);
        match feed.read_after(cursor, options.heartbeat) {
            Read::Entries(entries) if entries.is_empty() => {
                send(&mut w, &json!({"op": "heartbeat", "seq": cursor}))?;
            }
            Read::Entries(entries) => {
                out.clear();
                for (seq, line) in entries {
                    out.extend_from_slice(line.as_bytes());
                    out.push(b'\n');
                    cursor = seq;
                }
                w.write_all(&out)?;
                w.flush()?;
                sent.store(cursor, Ordering::Relaxed);
            }
            Read::Gap { from, to } => {
                // Too far behind: say what was lost, then resynchronise from a fresh snapshot.
                let (seq, snapshot) = feed.snapshot();
                send(&mut w, &json!({"op": "gap", "from": from, "to": to}))?;
                send(
                    &mut w,
                    &json!({"op": "snapshot", "seq": seq, "snapshot": snapshot}),
                )?;
                cursor = seq;
            }
        }
    }
    Ok(())
}
