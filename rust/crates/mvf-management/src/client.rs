//! A management client: connect, `hello`, snapshot, subscribe. The CLI uses it; any language can
//! speak the same newline-delimited JSON (`clients/mvf-manage-py` is a stdlib-only Python one).

use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

use crate::model::API;
use crate::sys::Conn;

/// A connected, greeted client.
pub struct Client {
    reader: BufReader<Box<dyn Conn>>,
    writer: Box<dyn Conn>,
    runtime: Value,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("runtime", &self.runtime)
            .finish()
    }
}

fn refused(v: &Value) -> std::io::Error {
    let code = v["code"].as_str().unwrap_or("error");
    let message = format!("{code}: {}", v["message"].as_str().unwrap_or_default());
    match code {
        "unauthorized" => std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
        _ => std::io::Error::other(message),
    }
}

impl Client {
    /// Connects to `endpoint` (a socket path or pipe name) and says hello as `name`.
    pub fn connect(endpoint: &str, name: &str) -> std::io::Result<Self> {
        let conn = crate::sys::connect(endpoint)?;
        let writer = conn.duplicate()?;
        let mut client = Self {
            reader: BufReader::new(conn),
            writer,
            runtime: Value::Null,
        };
        let reply = client.request(&json!({"op": "hello", "api": [API], "client": name}))?;
        if reply["op"] != "hello" {
            return Err(refused(&reply));
        }
        client.runtime = reply["runtime"].clone();
        Ok(client)
    }

    /// The runtime's identity from `hello`.
    pub fn runtime(&self) -> &Value {
        &self.runtime
    }

    fn read(&mut self) -> std::io::Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the runtime closed the connection",
            ));
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }

    /// Sends one request and reads one reply.
    pub fn request(&mut self, v: &Value) -> std::io::Result<Value> {
        let mut line = v.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        self.read()
    }

    /// The runtime's current state and the sequence it reflects.
    pub fn snapshot(&mut self) -> std::io::Result<(u64, Value)> {
        let reply = self.request(&json!({"op": "snapshot"}))?;
        if reply["op"] != "snapshot" {
            return Err(refused(&reply));
        }
        Ok((
            reply["seq"].as_u64().unwrap_or(0),
            reply["snapshot"].clone(),
        ))
    }

    /// Runs a command (`stop`, `pause`, `resume`, `set-value`, `configure`, `replace`, `rollback`,
    /// `commit`) under idempotency key `id`, waiting at most `deadline_ms` for the outcome.
    pub fn command(
        &mut self,
        id: &str,
        command: &str,
        args: Value,
        deadline_ms: Option<u64>,
    ) -> std::io::Result<Value> {
        let mut req = json!({"op": "command", "id": id, "command": command, "args": args});
        if let Some(d) = deadline_ms {
            req["deadlineMs"] = json!(d);
        }
        self.request(&req)
    }

    /// Subscribes from sequence `from` (inclusive), or, without one, from a fresh snapshot.
    pub fn subscribe(mut self, from: Option<u64>) -> std::io::Result<Subscription> {
        let mut req = json!({"op": "subscribe"});
        if let Some(f) = from {
            req["from"] = json!(f);
        }
        let mut line = req.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(Subscription {
            client: self,
            last: from.map(|f| f.saturating_sub(1)),
        })
    }
}

/// A live subscription: `entry`, `gap`, `snapshot` and `heartbeat` messages, in order.
#[derive(Debug)]
pub struct Subscription {
    client: Client,
    last: Option<u64>,
}

impl Subscription {
    /// The sequence of the last entry or snapshot received: resubscribe from `last + 1` after a
    /// disconnect to continue without loss or duplicates.
    pub fn last_seq(&self) -> Option<u64> {
        self.last
    }
}

impl Iterator for Subscription {
    type Item = std::io::Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        let v = match self.client.read() {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(e)),
        };
        match v["op"].as_str() {
            Some("entry") | Some("snapshot") => self.last = v["seq"].as_u64().or(self.last),
            Some("error") => return Some(Err(refused(&v))),
            _ => {}
        }
        Some(Ok(v))
    }
}
