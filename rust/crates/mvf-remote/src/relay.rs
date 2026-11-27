//! The relay: a separate, optional process that serves the management API to remote operators
//! over mutually authenticated TLS, as a client of the runtime's local endpoint (ADR-0007).
//!
//! Per connection:
//! 1. **TLS 1.3 handshake** within `handshake_timeout`, with a client certificate issued by the
//!    relay's CA and inside its validity dates.
//! 2. **Access list:** the certificate's fingerprint must be listed (and not revoked). The list is
//!    re-read for every connection. It gives the principal name and role.
//! 3. **Hello:** the relay opens its own connection to the local endpoint and says hello
//!    `onBehalfOf` the principal, so the runtime enforces the role a second time and audits the
//!    real principal.
//! 4. **Forwarding,** one request at a time. An observer's `command` is refused here, before it
//!    reaches the runtime. `subscribe` turns the connection into a one-way stream.
//!
//! A slow client is dropped by the write timeout; an idle one by the idle timeout. A relay crash
//! closes its local connections and nothing else: the runtime never depends on it.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use mvf_management::{Audit, Role};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};

use crate::access::{AccessList, Refusal};

/// Relay settings.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Address to listen on. Never defaulted: remote access is enabled by naming an interface.
    pub listen: SocketAddr,
    /// Local endpoint to relay. `None`: the only runtime registered for this user, found again
    /// for every connection, so the relay follows a restarted runtime.
    pub endpoint: Option<String>,
    /// CA certificate that client certificates must chain to.
    pub ca: PathBuf,
    /// The relay's certificate and key.
    pub cert: PathBuf,
    /// The relay's key.
    pub key: PathBuf,
    /// The access list.
    pub access: PathBuf,
    /// The relay's audit log (connections accepted and refused, forbidden requests).
    pub audit: Option<PathBuf>,
    /// Concurrent remote clients (default 8).
    pub max_clients: usize,
    /// TLS handshake limit (default 5 s).
    pub handshake_timeout: Duration,
    /// A connection idle between requests this long is closed (default 5 min).
    pub idle_timeout: Duration,
    /// A write blocked this long drops the client (default 5 s).
    pub write_timeout: Duration,
    /// Longest request line (default 64 KiB).
    pub max_request: usize,
    /// Longest request line for an operator (default 1 MiB: upload chunks).
    pub max_operator_request: usize,
}

impl RelayConfig {
    /// Settings from a PKI directory (`mvf-remote pki init`), listening on `listen`.
    pub fn from_pki(dir: &std::path::Path, listen: SocketAddr) -> Self {
        Self {
            listen,
            endpoint: None,
            ca: dir.join("ca.pem"),
            cert: dir.join("server.pem"),
            key: dir.join("server.key"),
            access: dir.join("access.json"),
            audit: Some(dir.join("relay-audit.jsonl")),
            max_clients: 8,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(300),
            write_timeout: Duration::from_secs(5),
            max_request: 64 * 1024,
            max_operator_request: 1024 * 1024,
        }
    }
}

/// Counters.
#[derive(Debug, Default)]
pub struct RelayCounters {
    /// Connections admitted.
    pub admitted: AtomicU64,
    /// Connections refused (handshake, access list, capacity, no runtime).
    pub refused: AtomicU64,
    /// Clients connected now.
    pub connected: AtomicUsize,
}

/// A running relay. Dropping it stops listening; open connections end when their clients go.
#[derive(Debug)]
pub struct Relay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    counters: Arc<RelayCounters>,
}

impl Relay {
    /// Loads the TLS identity and starts listening.
    pub fn start(config: RelayConfig) -> std::io::Result<Self> {
        let tls = crate::tls::server(&config.ca, &config.cert, &config.key)?;
        // Fail early on an unreadable access list rather than at the first connection.
        AccessList::load(&config.access)?;
        let audit = config
            .audit
            .as_deref()
            .map(Audit::open)
            .transpose()?
            .map(Arc::new);
        let listener = TcpListener::bind(config.listen)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(RelayCounters::default());
        let thread = {
            let (stop, counters) = (stop.clone(), counters.clone());
            std::thread::Builder::new()
                .name("mvf-remote".into())
                .spawn(move || accept(listener, &stop, &tls, &config, audit, &counters))?
        };
        Ok(Self {
            addr,
            stop,
            thread: Some(thread),
            counters,
        })
    }

    /// The address it listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Counters.
    pub fn counters(&self) -> &RelayCounters {
        &self.counters
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn record(audit: &Option<Arc<Audit>>, v: Value) {
    if let Some(a) = audit {
        a.record(v);
    }
}

fn accept(
    listener: TcpListener,
    stop: &AtomicBool,
    tls: &Arc<ServerConfig>,
    config: &RelayConfig,
    audit: Option<Arc<Audit>>,
    counters: &Arc<RelayCounters>,
) {
    for tcp in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(tcp) = tcp else { continue };
        let peer = tcp
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into());
        if counters.connected.load(Ordering::Relaxed) >= config.max_clients {
            counters.refused.fetch_add(1, Ordering::Relaxed);
            record(
                &audit,
                json!({"event": "connection-refused", "peer": peer, "reason": "busy"}),
            );
            continue;
        }
        counters.connected.fetch_add(1, Ordering::Relaxed);
        let (tls, config, audit, mine) =
            (tls.clone(), config.clone(), audit.clone(), counters.clone());
        let spawned = std::thread::Builder::new()
            .name("mvf-remote-client".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve(tcp, &peer, tls, &config, &audit, &mine)
                }));
                if let Ok(Err(reason)) = result {
                    record(
                        &audit,
                        json!({"event": "connection-ended", "peer": peer, "reason": reason}),
                    );
                }
                mine.connected.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            counters.connected.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

type Tls = StreamOwned<ServerConnection, TcpStream>;

fn send(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}

/// One line of at most `max` bytes; `Ok(None)` at end of stream.
fn read_line(r: &mut impl BufRead, max: usize) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf().map_err(|e| e.to_string())?;
        if buf.is_empty() {
            return Ok(None);
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..=i], true),
            None => (buf, false),
        };
        if line.len() + chunk.len() > max {
            return Err(format!("request over {max} bytes"));
        }
        line.extend_from_slice(chunk);
        let n = chunk.len();
        r.consume(n);
        if done {
            return Ok(Some(String::from_utf8_lossy(&line).trim().to_string()));
        }
    }
}

fn local_endpoint(config: &RelayConfig) -> Result<String, String> {
    if let Some(e) = &config.endpoint {
        return Ok(e.clone());
    }
    let dir = mvf_management::runtime_dir().map_err(|e| e.to_string())?;
    match mvf_management::list(&dir).as_slice() {
        [one] => Ok(one.endpoint.clone()),
        [] => Err("no runtime is running".into()),
        _ => Err("several runtimes are registered; start the relay with --endpoint".into()),
    }
}

fn serve(
    tcp: TcpStream,
    peer: &str,
    tls: Arc<ServerConfig>,
    config: &RelayConfig,
    audit: &Option<Arc<Audit>>,
    counters: &RelayCounters,
) -> Result<(), String> {
    let refuse = |reason: &str| {
        counters.refused.fetch_add(1, Ordering::Relaxed);
        record(
            audit,
            json!({"event": "connection-refused", "peer": peer, "reason": reason}),
        );
    };
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(config.handshake_timeout))
        .map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(config.write_timeout))
        .map_err(|e| e.to_string())?;
    let conn = ServerConnection::new(tls).map_err(|e| e.to_string())?;
    let mut stream: Tls = StreamOwned::new(conn, tcp);
    while stream.conn.is_handshaking() {
        if let Err(e) = stream.conn.complete_io(&mut stream.sock) {
            refuse(&format!("handshake: {e}"));
            return Ok(());
        }
    }
    let Some(fingerprint) = stream
        .conn
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| crate::pki::fingerprint(c.as_ref()))
    else {
        refuse("no client certificate");
        return Ok(());
    };
    let admitted = AccessList::load(&config.access)
        .map_err(|e| format!("access list: {e}"))
        .and_then(|l| {
            l.admit(&fingerprint).map_err(|r| match r {
                Refusal::Revoked => "revoked".to_string(),
                Refusal::Unknown => "not in the access list".to_string(),
                Refusal::BadRole(r) => format!("unknown role {r:?}"),
            })
        });
    let (principal, role) = match admitted {
        Ok(x) => x,
        Err(reason) => {
            refuse(&format!("{reason} ({fingerprint})"));
            let _ = send(
                &mut stream,
                &json!({"op": "error", "code": "unauthorized", "message": "this certificate is not admitted"}),
            );
            return Ok(());
        }
    };
    stream
        .sock
        .set_read_timeout(Some(config.idle_timeout))
        .map_err(|e| e.to_string())?;
    let mut client = BufReader::new(stream);

    // The client's hello, then the relay's own, on its behalf, to the runtime.
    let Some(line) = read_line(&mut client, config.max_request)? else {
        return Ok(());
    };
    let mut hello: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if hello["op"] != "hello" {
        let _ = send(
            client.get_mut(),
            &json!({"op": "error", "code": "hello-first", "message": "send hello first"}),
        );
        return Ok(());
    }
    let endpoint = match local_endpoint(config) {
        Ok(e) => e,
        Err(reason) => {
            refuse(&reason);
            let _ = send(
                client.get_mut(),
                &json!({"op": "error", "code": "unavailable", "message": reason}),
            );
            return Ok(());
        }
    };
    let local =
        mvf_management::connect_raw(&endpoint).map_err(|e| format!("local endpoint: {e}"))?;
    let mut to_runtime = local.duplicate().map_err(|e| e.to_string())?;
    let mut from_runtime = BufReader::new(local);
    hello["onBehalfOf"] =
        json!({"principal": principal, "role": role.as_str(), "via": format!("mvf-remote {peer}")});
    send(&mut to_runtime, &hello).map_err(|e| e.to_string())?;
    let reply = read_line(&mut from_runtime, usize::MAX)?.ok_or("the runtime closed")?;
    client
        .get_mut()
        .write_all(format!("{reply}\n").as_bytes())
        .and_then(|_| client.get_mut().flush())
        .map_err(|e| e.to_string())?;
    counters.admitted.fetch_add(1, Ordering::Relaxed);
    record(
        audit,
        json!({"event": "connection-admitted", "peer": peer, "principal": principal, "role": role.as_str()}),
    );

    let limit = if role == Role::Operator {
        config.max_operator_request
    } else {
        config.max_request
    };
    loop {
        let line = match read_line(&mut client, limit) {
            Ok(Some(l)) if l.is_empty() => continue,
            Ok(Some(l)) => l,
            Ok(None) => return Ok(()),
            Err(e) => {
                let _ = send(
                    client.get_mut(),
                    &json!({"op": "error", "code": "too-large", "message": e}),
                );
                return Ok(());
            }
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            let _ = send(
                client.get_mut(),
                &json!({"op": "error", "code": "malformed", "message": "one JSON object per line"}),
            );
            return Ok(());
        };
        let op = msg["op"].as_str().unwrap_or_default();
        let needs = match op {
            "command" | "upload" | "upload-status" => Role::Operator,
            "preview" => Role::Previewer,
            _ => Role::Observer,
        };
        if role < needs {
            record(
                audit,
                json!({"event": "forbidden", "peer": peer, "principal": principal, "role": role.as_str(), "op": op, "command": msg["command"], "id": msg["id"]}),
            );
            let message = format!("the {} role cannot {op}", role.as_str());
            let reply = if op == "command" {
                json!({"op": "command", "id": msg["id"], "command": msg["command"], "status": "refused", "code": "forbidden", "message": message})
            } else {
                json!({"op": "error", "code": "forbidden", "message": message})
            };
            send(client.get_mut(), &reply).map_err(|e| e.to_string())?;
            continue;
        }
        send(&mut to_runtime, &msg).map_err(|e| e.to_string())?;
        if op == "subscribe" || op == "preview" {
            // From here on: runtime -> client only. Heartbeats from the runtime keep it moving
            // and expose a dead client through a failed write.
            loop {
                let Some(line) = read_line(&mut from_runtime, usize::MAX)? else {
                    return Ok(());
                };
                let w = client.get_mut();
                if w.write_all(line.as_bytes())
                    .and_then(|_| w.write_all(b"\n"))
                    .and_then(|_| w.flush())
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
        let reply = read_line(&mut from_runtime, usize::MAX)?.ok_or("the runtime closed")?;
        let w = client.get_mut();
        w.write_all(reply.as_bytes())
            .and_then(|_| w.write_all(b"\n"))
            .and_then(|_| w.flush())
            .map_err(|e| e.to_string())?;
    }
}
