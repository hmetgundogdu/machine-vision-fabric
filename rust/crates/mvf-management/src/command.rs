//! Commands (#33 part 2): authorized, idempotent, bounded by a deadline, and audited.
//!
//! A command never bypasses the engine. It becomes the same validated step as an update plan
//! (`parse_update_plan`) and is applied by the engine at a cycle boundary through its
//! [`ControlHandle`], or, for `stop`, through the run's cancellation token (a clean stop at the
//! next boundary). The reply reports what the engine did, read from the feed's own events:
//!
//! | `status` | Meaning |
//! |---|---|
//! | `applied` | the engine reported the effect (`event` holds its record) |
//! | `rejected` | the engine refused it; nothing changed (`event` says why) |
//! | `pending` | sent, but no outcome before the deadline: follow the stream from `afterSeq` |
//! | `refused` | not sent: wrong role, unknown command, invalid arguments, no control attached |
//!
//! A repeated `id` from the same principal is never sent again: the first reply comes back with
//! `"duplicate": true`. Every command, refused ones included, is appended to the audit log.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvf_engine::{CancellationToken, ControlHandle, PlannedCommand, parse_update_plan};
use mvf_graph::ContractResolver;
use serde_json::{Value, json};

use crate::feed::Feed;

/// What a session may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Topology, state, events and logs.
    Observer,
    /// Observer plus commands.
    Operator,
}

impl Role {
    /// Parses `observer` / `operator`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "observer" => Some(Role::Observer),
            "operator" => Some(Role::Operator),
            _ => None,
        }
    }

    /// The role's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Observer => "observer",
            Role::Operator => "operator",
        }
    }
}

/// Who is asking, as the server established it.
#[derive(Debug, Clone)]
pub struct Session {
    /// The principal: `local` for the endpoint's owner, else the name a relay vouched for.
    pub principal: String,
    /// What it may do.
    pub role: Role,
    /// How it arrived (`local`, or the relay and the remote peer).
    pub via: String,
}

impl Session {
    /// The endpoint's owner: an operator.
    pub fn local() -> Self {
        Self {
            principal: "local".into(),
            role: Role::Operator,
            via: "local".into(),
        }
    }

    /// The session a `hello` asks for. A relay running as the owner may speak for a remote
    /// principal, at most with the owner's role; anything it claims beyond that is ignored.
    pub fn from_hello(hello: &Value) -> Result<Self, String> {
        let behalf = &hello["onBehalfOf"];
        if behalf.is_null() {
            return Ok(Self::local());
        }
        let principal = behalf["principal"]
            .as_str()
            .filter(|p| !p.is_empty() && p.len() <= 128)
            .ok_or("onBehalfOf.principal must be a name of at most 128 characters")?;
        let role = behalf["role"]
            .as_str()
            .and_then(Role::parse)
            .ok_or("onBehalfOf.role must be observer or operator")?;
        Ok(Self {
            principal: principal.to_string(),
            role: role.min(Role::Operator),
            via: behalf["via"]
                .as_str()
                .unwrap_or("relay")
                .chars()
                .take(200)
                .collect(),
        })
    }
}

/// What the server needs to carry out commands for a run.
#[derive(Clone)]
pub struct Control {
    /// The engine's command channel.
    pub handle: ControlHandle,
    /// The run's cancellation token (`stop`).
    pub cancel: CancellationToken,
    /// Resolves replacement candidates: builtins, installed modules, contracts inside the
    /// package. The host's own resolver, so commands can name nothing a plan file could not.
    pub resolver: Arc<dyn ContractResolver + Send + Sync>,
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control").finish_non_exhaustive()
    }
}

/// An append-only JSON-lines audit log, private to the user.
#[derive(Debug)]
pub struct Audit {
    file: Mutex<std::fs::File>,
    path: PathBuf,
}

impl Audit {
    /// Opens (creating) the log at `path`.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: Mutex::new(options.open(path)?),
            path: path.to_path_buf(),
        })
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record and flushes it to disk (commands are rare; the record must survive).
    pub fn record(&self, mut v: Value) {
        v["atMs"] = json!(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        );
        let mut line = v.to_string();
        line.push('\n');
        if let Ok(mut f) = self.file.lock() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.sync_data();
        }
    }
}

enum Done {
    InProgress,
    Reply(Value),
}

/// (principal, command id).
type Key = (String, String);

/// Remembered commands, and their order for eviction.
#[derive(Default)]
struct Seen {
    done: HashMap<Key, Done>,
    order: VecDeque<Key>,
}

/// Carries out commands for one server.
pub struct Commands {
    control: Option<Control>,
    audit: Option<Arc<Audit>>,
    seen: Mutex<Seen>,
}

impl std::fmt::Debug for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Commands")
            .field("control", &self.control.is_some())
            .finish_non_exhaustive()
    }
}

/// Command ids remembered per server for duplicate detection.
const REMEMBERED: usize = 4096;
/// Default and largest command deadline.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
const MAX_DEADLINE: Duration = Duration::from_secs(60);

fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

impl Commands {
    /// Commands for a run (`control`), audited to `audit`.
    pub fn new(control: Option<Control>, audit: Option<Arc<Audit>>) -> Self {
        Self {
            control,
            audit,
            seen: Mutex::new(Seen::default()),
        }
    }

    /// Whether this server can carry out commands at all.
    pub fn available(&self) -> bool {
        self.control.is_some()
    }

    /// The audit log, if any.
    pub fn audit(&self) -> Option<&Arc<Audit>> {
        self.audit.as_ref()
    }

    fn reply(id: &str, command: &str, status: &str) -> Value {
        json!({"op": "command", "id": id, "command": command, "status": status})
    }

    fn refused(id: &str, command: &str, code: &str, message: impl Into<String>) -> Value {
        let mut r = Self::reply(id, command, "refused");
        r["code"] = json!(code);
        r["message"] = json!(message.into());
        r
    }

    /// Carries out one `{"op":"command",…}` request for `session`.
    pub fn execute(&self, feed: &Feed, session: &Session, msg: &Value) -> Value {
        let id = msg["id"].as_str().unwrap_or_default().to_string();
        let command = msg["command"].as_str().unwrap_or_default().to_string();
        let reply = self.decide(feed, session, &id, &command, msg);
        if let Some(audit) = &self.audit {
            audit.record(json!({
                "principal": session.principal,
                "role": session.role.as_str(),
                "via": session.via,
                "id": id,
                "command": command,
                "args": msg["args"],
                "status": reply["status"],
                "duplicate": reply["duplicate"],
                "code": reply["code"],
                "event": reply["event"],
                "afterSeq": reply["afterSeq"],
            }));
        }
        reply
    }

    fn decide(
        &self,
        feed: &Feed,
        session: &Session,
        id: &str,
        command: &str,
        msg: &Value,
    ) -> Value {
        if !valid_id(id) {
            return Self::refused(
                id,
                command,
                "id-required",
                "a command needs an `id` of 1-128 characters [A-Za-z0-9-_.:], unique per command",
            );
        }
        if session.role < Role::Operator {
            return Self::refused(
                id,
                command,
                "forbidden",
                format!("the {} role cannot run commands", session.role.as_str()),
            );
        }
        let Some(control) = &self.control else {
            return Self::refused(id, command, "unavailable", "this runtime takes no commands");
        };
        // At most once per (principal, id).
        let key = (session.principal.clone(), id.to_string());
        {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            match seen.done.get(&key) {
                Some(Done::Reply(r)) => {
                    let mut r = r.clone();
                    r["duplicate"] = json!(true);
                    return r;
                }
                Some(Done::InProgress) => {
                    let mut r = Self::reply(id, command, "pending");
                    r["duplicate"] = json!(true);
                    return r;
                }
                None => {
                    seen.done.insert(key.clone(), Done::InProgress);
                    seen.order.push_back(key.clone());
                    while seen.order.len() > REMEMBERED {
                        if let Some(old) = seen.order.pop_front() {
                            seen.done.remove(&old);
                        }
                    }
                }
            }
        }
        let deadline = msg["deadlineMs"]
            .as_u64()
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_DEADLINE)
            .min(MAX_DEADLINE);
        let reply = self.run(feed, control, id, command, &msg["args"], deadline);
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.done.insert(key, Done::Reply(reply.clone()));
        reply
    }

    fn run(
        &self,
        feed: &Feed,
        control: &Control,
        id: &str,
        command: &str,
        args: &Value,
        deadline: Duration,
    ) -> Value {
        let node = args["node"].as_str().unwrap_or_default().to_string();
        let until = Instant::now() + deadline;
        let after = feed.last_seq();
        if command == "stop" {
            control.cancel.cancel();
            let outcome = feed.wait_for(after, until, |e| {
                (e["kind"] == "status" && e["status"] == "stopped").then(|| ("applied", e.clone()))
            });
            return Self::outcome(id, command, after, outcome);
        }
        // Everything else is an update-plan step, validated exactly like a plan file.
        let mut step = json!({"node": node, "atCycle": 1});
        let field = |k: &str| args.get(k).filter(|v| !v.is_null()).cloned();
        let action = match command {
            "set-value" => "set",
            "configure" => "configure",
            "pause" => "pause",
            "resume" => "resume",
            "replace" => "replace",
            "rollback" => "rollback",
            "commit" => "commit",
            other => {
                return Self::refused(
                    id,
                    other,
                    "unknown-command",
                    "commands: stop, pause, resume, set-value, configure, replace, rollback, commit",
                );
            }
        };
        step["action"] = json!(action);
        for k in [
            "value",
            "field",
            "forMs",
            "builtin",
            "module",
            "contract",
            "config",
            "probationCycles",
            "storage",
        ] {
            if let Some(v) = field(k) {
                step[k] = v;
            }
        }
        let plan = json!({"apiVersion": "mvf.dev/v1alpha1", "kind": "UpdatePlan", "spec": {"updates": [step]}});
        let planned = match parse_update_plan(&plan.to_string(), control.resolver.as_ref()) {
            Ok(mut steps) if steps.len() == 1 => steps.remove(0).1,
            Ok(_) => return Self::refused(id, command, "invalid", "expected one step"),
            Err(e) => return Self::refused(id, command, "invalid", e),
        };
        if !planned.submit(&control.handle) {
            return Self::refused(id, command, "unavailable", "the run has ended");
        }
        let outcome = feed.wait_for(after, until, |e| Self::matches(&planned, e));
        Self::outcome(id, command, after, outcome)
    }

    fn outcome(id: &str, command: &str, after: u64, outcome: Option<(&str, Value)>) -> Value {
        let mut r = match &outcome {
            Some((status, _)) => Self::reply(id, command, status),
            None => Self::reply(id, command, "pending"),
        };
        r["afterSeq"] = json!(after);
        if let Some((_, event)) = outcome {
            r["event"] = event;
        }
        r
    }

    /// Whether entry `e` is the engine's answer to `planned`.
    fn matches(planned: &PlannedCommand, e: &Value) -> Option<(&'static str, Value)> {
        if e["kind"] != "event" {
            return None;
        }
        let d = &e["detail"];
        let kind = d["kind"].as_str().unwrap_or_default();
        let node_is = |n: &str| d["node"].as_str() == Some(n);
        let verdict = match (planned, e["event"].as_str()) {
            (PlannedCommand::SetValue(n, _), Some("live")) if node_is(n) => match kind {
                "value-set" => Some("applied"),
                "rejected" => Some("rejected"),
                _ => None,
            },
            (PlannedCommand::Configure(n, f, _), Some("live"))
                if node_is(n) && d["field"].as_str() == Some(f.as_str()) =>
            {
                match kind {
                    "reconfigured" | "reopened" => Some("applied"),
                    "rejected" => Some("rejected"),
                    _ => None,
                }
            }
            (PlannedCommand::Pause(_), Some("live")) => match kind {
                "paused" => Some("applied"),
                "rejected" if d["node"].is_null() => Some("rejected"),
                _ => None,
            },
            (PlannedCommand::Resume, Some("live")) => match kind {
                "resumed" => Some("applied"),
                "rejected" if d["node"].is_null() => Some("rejected"),
                _ => None,
            },
            (PlannedCommand::Replace(r), Some("replacement")) if node_is(&r.node) => match kind {
                "promoted" => Some("applied"),
                "rejected" | "rolled-back" => Some("rejected"),
                _ => None,
            },
            (PlannedCommand::Rollback(n), Some("replacement")) if node_is(n) => match kind {
                "rolled-back" => Some("applied"),
                "rejected" => Some("rejected"),
                _ => None,
            },
            (PlannedCommand::Commit(n), Some("replacement")) if node_is(n) => match kind {
                "committed" => Some("applied"),
                "rejected" => Some("rejected"),
                _ => None,
            },
            _ => None,
        };
        verdict.map(|v| (v, d.clone()))
    }
}
