//! A remote client: the management API over the relay's mutually authenticated TLS. The messages
//! are exactly those of the local endpoint (`protocol/management-v1.md`).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use serde_json::{Value, json};

type Tls = StreamOwned<ClientConnection, TcpStream>;

/// A connected, greeted remote client.
#[derive(Debug)]
pub struct RemoteClient {
    stream: BufReader<Tls>,
    hello: Value,
}

fn refused(v: &Value) -> std::io::Error {
    let code = v["code"].as_str().unwrap_or("error");
    let message = format!("{code}: {}", v["message"].as_str().unwrap_or_default());
    if code == "unauthorized" {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, message)
    } else {
        std::io::Error::other(message)
    }
}

impl RemoteClient {
    /// Connects to the relay at `addr` (`host:port`; `host` must match its certificate) and says
    /// hello as `name`.
    pub fn connect(addr: &str, tls: Arc<ClientConfig>, name: &str) -> std::io::Result<Self> {
        let host = addr
            .rsplit_once(':')
            .map(|(h, _)| h.trim_start_matches('[').trim_end_matches(']'))
            .ok_or_else(|| std::io::Error::other("expected host:port"))?;
        let server = ServerName::try_from(host.to_string()).map_err(std::io::Error::other)?;
        let tcp = TcpStream::connect(addr)?;
        tcp.set_nodelay(true)?;
        let conn = ClientConnection::new(tls, server).map_err(std::io::Error::other)?;
        let mut client = Self {
            stream: BufReader::new(StreamOwned::new(conn, tcp)),
            hello: Value::Null,
        };
        let reply = client.request(&json!({"op": "hello", "api": [1], "client": name}))?;
        if reply["op"] != "hello" {
            return Err(refused(&reply));
        }
        client.hello = reply;
        Ok(client)
    }

    /// The runtime's `hello` as relayed: runtime identity, the principal and role the relay
    /// admitted, the features offered.
    pub fn hello(&self) -> &Value {
        &self.hello
    }

    fn read(&mut self) -> std::io::Result<Value> {
        let mut line = String::new();
        if self.stream.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the relay closed the connection",
            ));
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }

    /// Sends one request and reads one reply.
    pub fn request(&mut self, v: &Value) -> std::io::Result<Value> {
        let w = self.stream.get_mut();
        w.write_all(format!("{v}\n").as_bytes())?;
        w.flush()?;
        self.read()
    }

    /// The runtime's current state and its sequence.
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

    /// Runs a command (see `mvf_management::command`).
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

    /// Subscribes from `from` (inclusive), or from a fresh snapshot.
    pub fn subscribe(mut self, from: Option<u64>) -> std::io::Result<RemoteSubscription> {
        let mut req = json!({"op": "subscribe"});
        if let Some(f) = from {
            req["from"] = json!(f);
        }
        let w = self.stream.get_mut();
        w.write_all(format!("{req}\n").as_bytes())?;
        w.flush()?;
        Ok(RemoteSubscription {
            client: self,
            last: from.map(|f| f.saturating_sub(1)),
        })
    }
}

/// A live remote subscription.
#[derive(Debug)]
pub struct RemoteSubscription {
    client: RemoteClient,
    last: Option<u64>,
}

impl RemoteSubscription {
    /// The last entry or snapshot sequence received; resume from `last + 1`.
    pub fn last_seq(&self) -> Option<u64> {
        self.last
    }
}

impl Iterator for RemoteSubscription {
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
