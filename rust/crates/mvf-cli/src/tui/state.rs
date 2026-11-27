//! The TUI's model (#35): what the management stream says, plus the operator's place in it.
//!
//! - **Pure:** no I/O and no terminal, so tests drive it with real snapshots and entries.
//! - **Bounded:**
//!   - logs: 300 overall, 30 per node;
//!   - events: 200;
//!   - latency history: 120 samples per node;
//!   - previews: only the latest.
//! - **Gaps:** recovering from a gap is the stream's job: a `gap` is followed by a fresh
//!   snapshot, which replaces everything.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};

use super::image::Image;
use super::layout::{EdgeIn, Layout};

const LOGS: usize = 300;
const NODE_LOGS: usize = 30;
const EVENTS: usize = 200;
const HISTORY: usize = 120;

/// A port.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    /// Name.
    pub name: String,
    /// Payload type.
    pub ty: String,
    /// `data` or `control`.
    pub channel: String,
}

/// A live-editable field (from the topology).
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Field (`value` for a `value` node).
    pub field: String,
    /// `configure` or `set-value`.
    pub command: String,
    /// Element type.
    pub elem: String,
    /// `one` or `list`.
    pub shape: String,
    /// JSON Schema, when declared.
    pub schema: Option<Value>,
    /// Binding key (persisted edits).
    pub binding: Option<String>,
}

/// A node as the topology describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Instance id.
    pub id: String,
    /// Contract `id@version` at start.
    pub contract: String,
    /// The primitive, if any.
    pub primitive: Option<String>,
    /// Backend at start.
    pub backend: String,
    /// Instances.
    pub parallelism: u64,
    /// Inputs.
    pub inputs: Vec<Port>,
    /// Outputs.
    pub outputs: Vec<Port>,
    /// Editable fields.
    pub fields: Vec<Field>,
}

impl Node {
    /// Colour family, as in the legacy dashboard.
    pub fn category(&self) -> &'static str {
        match self.primitive.as_deref() {
            Some("value") => "value",
            Some(_) => "flow",
            None if self.inputs.is_empty() => "source",
            None if self.outputs.is_empty() => "sink",
            None if self
                .outputs
                .iter()
                .any(|p| p.channel == "control" || p.ty == "classification") =>
            {
                "classify"
            }
            None => "compute",
        }
    }
}

/// An edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Source node.
    pub from: String,
    /// Source port.
    pub from_port: String,
    /// Target node.
    pub to: String,
    /// Target port.
    pub to_port: String,
    /// Payload type.
    pub ty: String,
    /// A control edge.
    pub control: bool,
}

/// Live state of a node, from deltas.
#[derive(Debug, Clone, Default)]
pub struct Live {
    /// Generation.
    pub generation: u64,
    /// Executions.
    pub executed: u64,
    /// Skips.
    pub skipped: u64,
    /// Executing right now (reported by the engine, not inferred).
    pub active: bool,
    /// Executions in flight (replicated stages).
    pub running: u64,
    /// Failures handled by the recovery policy.
    pub failures: u64,
    /// Restarts.
    pub restarts: u64,
    /// Cycles dropped by backpressure.
    pub dropped: u64,
    /// The last skip reason.
    pub last_skip: Option<String>,
    /// Latency (µs) over the run.
    pub p50: f64,
    /// p95.
    pub p95: f64,
    /// p99.
    pub p99: f64,
    /// Maximum.
    pub max: f64,
    /// Mean.
    pub mean: f64,
    /// Executions per second in the last window.
    pub per_second: f64,
    /// Active contract after a replacement or rollback.
    pub contract: Option<String>,
    /// Active backend after a replacement or rollback.
    pub backend: Option<String>,
    /// When `executed` last went up (for the afterglow).
    pub fired: Option<Instant>,
    /// Window mean latency per delta, newest last.
    pub history: VecDeque<u64>,
}

/// A log line.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Node, when the line is a node's.
    pub node: Option<String>,
    /// Level (`info`, `warn`, `error`, `stderr`, `supervisor`, …).
    pub level: String,
    /// Text.
    pub message: String,
}

/// How an event line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Neutral.
    Info,
    /// Went well (promoted, committed, applied).
    Good,
    /// Worth a look (rolled back, skipped, paused).
    Warn,
    /// Went wrong (rejected, failed, restart).
    Bad,
}

/// An event line.
#[derive(Debug, Clone)]
pub struct EventLine {
    /// Node.
    pub node: Option<String>,
    /// Text.
    pub text: String,
    /// Tone.
    pub tone: Tone,
}

/// Which screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The graph.
    Graph,
    /// One node in detail.
    Detail,
}

/// The connection to the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    /// Not connected yet.
    Connecting,
    /// Subscribed.
    Live,
    /// Lost; retrying.
    Lost(String),
}

/// What the event loop should do after a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing outside the model.
    None,
    /// Leave the TUI.
    Quit,
    /// Send a management command.
    Command {
        /// Command id.
        id: String,
        /// `pause`, `resume`, `configure`, `set-value`.
        command: String,
        /// Its args.
        args: Value,
    },
    /// Start (`Some(node)`) or stop (`None`) previewing.
    Preview(Option<String>),
}

/// The current preview.
#[derive(Debug, Clone, Default)]
pub struct PreviewView {
    /// The node being previewed.
    pub node: Option<String>,
    /// The latest decoded image and its cycle.
    pub image: Option<(u64, Image)>,
    /// Why there is no image (not enabled, unsupported payload, …).
    pub note: Option<String>,
}

/// The whole model.
#[derive(Debug)]
pub struct App {
    /// `runtime` from the hello or snapshot.
    pub runtime: Value,
    /// `starting`, `running`, `paused`, `stopped`.
    pub status: String,
    /// End reason once stopped.
    pub end: Option<Value>,
    /// Completed cycles.
    pub cycle: u64,
    /// Pipeline name.
    pub pipeline: String,
    /// Nodes in declaration order.
    pub nodes: Vec<Node>,
    /// Edges.
    pub edges: Vec<Edge>,
    /// Layout of `nodes` and `edges`.
    pub layout: Layout,
    /// Live state per node id.
    pub live: HashMap<String, Live>,
    /// Recent log lines (all nodes).
    pub logs: VecDeque<LogLine>,
    /// Recent log lines per node.
    pub node_logs: HashMap<String, VecDeque<String>>,
    /// Recent events.
    pub events: VecDeque<EventLine>,
    /// Log lines the runtime refused (telemetry budget).
    pub dropped_logs: u64,
    /// Gaps recovered through a snapshot.
    pub gaps: u64,
    /// Connection state.
    pub conn: Conn,
    /// Whether this session may send commands.
    pub commands: bool,
    /// Selected node index.
    pub selected: Option<usize>,
    /// The selection follows the active node.
    pub follow: bool,
    /// Current screen.
    pub view: View,
    /// Help overlay.
    pub help: bool,
    /// Selected editable field in the detail view.
    pub field: usize,
    /// Text being typed for the selected field.
    pub editing: Option<String>,
    /// One-line notice (command results, validation errors).
    pub notice: Option<(String, Tone)>,
    /// Viewport origin on the graph canvas.
    pub scroll: (u16, u16),
    /// The viewport was panned by hand: it stays until the selection moves.
    pub pinned: bool,
    /// Preview.
    pub preview: PreviewView,
    /// Edits sent from here, by id: (node, field, value text).
    pub sent: HashMap<String, (String, String, String)>,
    /// Last value applied from this session, per (node, field).
    pub applied: HashMap<(String, String), String>,
    /// For the cycle rate.
    rate: Option<(Instant, u64)>,
    /// Cycles per second.
    pub cycles_per_s: f64,
    next_id: u64,
    session: String,
}

fn ports(v: &Value) -> Vec<Port> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| Port {
                    name: p["name"].as_str().unwrap_or_default().into(),
                    ty: p["type"].as_str().unwrap_or_default().into(),
                    channel: p["channel"].as_str().unwrap_or("data").into(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn split(endpoint: &str) -> (String, String) {
    match endpoint.split_once('.') {
        Some((n, p)) => (n.into(), p.into()),
        None => (endpoint.into(), String::new()),
    }
}

/// Text from the runtime with control characters replaced: a log line must never reach the
/// terminal as an escape sequence.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Formats microseconds the way the legacy dashboard did (µs, ms, s, m).
pub fn duration(us: f64) -> String {
    if us <= 0.0 {
        "-".into()
    } else if us < 1_000.0 {
        format!("{us:.0}us")
    } else if us < 1_000_000.0 {
        format!("{:.1}ms", us / 1e3)
    } else if us < 60e6 {
        format!("{:.2}s", us / 1e6)
    } else {
        format!("{:.1}m", us / 60e6)
    }
}

impl App {
    /// An empty model; `session` makes command ids unique per TUI process.
    pub fn new(session: &str) -> Self {
        Self {
            runtime: Value::Null,
            status: "starting".into(),
            end: None,
            cycle: 0,
            pipeline: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            layout: Layout::default(),
            live: HashMap::new(),
            logs: VecDeque::new(),
            node_logs: HashMap::new(),
            events: VecDeque::new(),
            dropped_logs: 0,
            gaps: 0,
            conn: Conn::Connecting,
            commands: false,
            selected: None,
            follow: true,
            view: View::Graph,
            help: false,
            field: 0,
            editing: None,
            notice: None,
            scroll: (0, 0),
            pinned: false,
            preview: PreviewView::default(),
            sent: HashMap::new(),
            applied: HashMap::new(),
            rate: None,
            cycles_per_s: 0.0,
            next_id: 0,
            session: session.into(),
        }
    }

    /// Live state of `id` (default when nothing arrived yet).
    pub fn live_of(&self, id: &str) -> Live {
        self.live.get(id).cloned().unwrap_or_default()
    }

    /// The node the run ended on, and why: (`node`, `exhausted` or `failed`).
    pub fn end_node(&self) -> Option<(String, &'static str)> {
        let end = self.end.as_ref()?;
        let node = end["node"].as_str()?.to_string();
        match end["reason"].as_str()? {
            "source-exhausted" => Some((node, "exhausted")),
            "node-failed" | "start-failed" => Some((node, "failed")),
            _ => None,
        }
    }

    /// The hello reply: runtime info and whether commands are allowed.
    pub fn hello(&mut self, hello: &Value) {
        self.runtime = hello["runtime"].clone();
        self.commands = hello["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x == "commands"));
    }

    /// Applies one message from the subscription.
    pub fn apply(&mut self, msg: &Value) {
        match msg["op"].as_str() {
            Some("snapshot") => self.snapshot(&msg["snapshot"]),
            Some("entry") => self.entry(&msg["entry"]),
            Some("gap") => {
                self.gaps += 1;
                self.event(
                    None,
                    "observation gap: reloading from a snapshot".into(),
                    Tone::Warn,
                );
            }
            _ => {}
        }
    }

    fn snapshot(&mut self, s: &Value) {
        if !s["runtime"].is_null() {
            self.runtime = s["runtime"].clone();
        }
        self.status = s["status"].as_str().unwrap_or("running").into();
        self.end = (!s["end"].is_null()).then(|| s["end"].clone());
        self.cycle = s["cycle"].as_u64().unwrap_or(0);
        self.topology(&s["topology"]);
        self.live.clear();
        if let Some(nodes) = s["nodes"].as_object() {
            for (id, v) in nodes {
                self.delta_node(id, v);
            }
        }
        self.activity(&s["activity"]);
        self.logs.clear();
        self.node_logs.clear();
        for l in s["recentLogs"].as_array().into_iter().flatten() {
            self.log(l);
        }
        self.events.clear();
        for e in s["recentEvents"].as_array().into_iter().flatten() {
            self.engine_event(e);
        }
    }

    fn topology(&mut self, t: &Value) {
        let nodes: Vec<Node> = t["nodes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| Node {
                        id: n["id"].as_str().unwrap_or_default().into(),
                        contract: n["contract"].as_str().unwrap_or_default().into(),
                        primitive: n["primitive"].as_str().map(str::to_string),
                        backend: n["backend"].as_str().unwrap_or_default().into(),
                        parallelism: n["parallelism"].as_u64().unwrap_or(1),
                        inputs: ports(&n["inputs"]),
                        outputs: ports(&n["outputs"]),
                        fields: n["live"]
                            .as_array()
                            .map(|f| {
                                f.iter()
                                    .map(|f| Field {
                                        field: f["field"].as_str().unwrap_or_default().into(),
                                        command: f["command"]
                                            .as_str()
                                            .unwrap_or("configure")
                                            .into(),
                                        elem: f["type"].as_str().unwrap_or("json").into(),
                                        shape: f["shape"].as_str().unwrap_or("one").into(),
                                        schema: (!f["schema"].is_null())
                                            .then(|| f["schema"].clone()),
                                        binding: f["binding"].as_str().map(str::to_string),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut edges = Vec::new();
        for (kind, control) in [("data", false), ("control", true)] {
            for e in t["edges"][kind].as_array().into_iter().flatten() {
                let (from, from_port) = split(e["from"].as_str().unwrap_or_default());
                let (to, to_port) = split(e["to"].as_str().unwrap_or_default());
                edges.push(Edge {
                    from,
                    from_port,
                    to,
                    to_port,
                    ty: e["type"].as_str().unwrap_or_default().into(),
                    control,
                });
            }
        }
        if nodes == self.nodes && edges == self.edges {
            return;
        }
        self.pipeline = t["pipeline"].as_str().unwrap_or_default().into();
        let index = |id: &str| nodes.iter().position(|n| n.id == id);
        let layout_edges: Vec<EdgeIn> = edges
            .iter()
            .filter_map(|e| {
                Some(EdgeIn {
                    from: index(&e.from)?,
                    to: index(&e.to)?,
                    control: e.control,
                    label: e.from_port.clone(),
                })
            })
            .collect();
        self.layout = Layout::build(nodes.len(), &layout_edges);
        let keep = self
            .selected
            .and_then(|i| self.nodes.get(i))
            .and_then(|n| nodes.iter().position(|m| m.id == n.id));
        self.nodes = nodes;
        self.edges = edges;
        self.selected = keep.or_else(|| self.layout.order.first().copied());
    }

    fn delta_node(&mut self, id: &str, v: &Value) {
        let l = self.live.entry(id.to_string()).or_default();
        let executed = v["executed"].as_u64().unwrap_or(l.executed);
        if executed > l.executed {
            l.fired = Some(Instant::now());
        }
        l.executed = executed;
        l.generation = v["generation"].as_u64().unwrap_or(l.generation);
        l.skipped = v["skipped"].as_u64().unwrap_or(l.skipped);
        l.failures = v["failures"].as_u64().unwrap_or(l.failures);
        l.restarts = v["restarts"].as_u64().unwrap_or(l.restarts);
        l.dropped = v["dropped"].as_u64().unwrap_or(l.dropped);
        l.last_skip = match &v["lastSkip"] {
            Value::Null => None,
            s => Some(match (s["reason"].as_str(), s["port"].as_str()) {
                (Some(r), Some(p)) => format!("{r} ({p})"),
                (Some(r), None) => r.to_string(),
                _ => s.to_string(),
            }),
        };
        let lat = &v["latencyUs"];
        l.p50 = lat["p50"].as_f64().unwrap_or(0.0);
        l.p95 = lat["p95"].as_f64().unwrap_or(0.0);
        l.p99 = lat["p99"].as_f64().unwrap_or(0.0);
        l.max = lat["max"].as_f64().unwrap_or(0.0);
        l.mean = lat["mean"].as_f64().unwrap_or(0.0);
        l.per_second = v["window"]["perSecond"].as_f64().unwrap_or(0.0);
        if v["window"]["executed"].as_u64().unwrap_or(0) > 0 {
            l.history
                .push_back(v["window"]["meanUs"].as_f64().unwrap_or(0.0) as u64);
            while l.history.len() > HISTORY {
                l.history.pop_front();
            }
        }
        if let Some(c) = v["contract"].as_str() {
            l.contract = Some(c.into());
        }
        if let Some(b) = v["backend"].as_str() {
            l.backend = Some(b.into());
        }
    }

    /// Executions in progress per node, as the runtime sampled them.
    fn activity(&mut self, running: &Value) {
        for (id, n) in running.as_object().into_iter().flatten() {
            let l = self.live.entry(id.clone()).or_default();
            l.running = n.as_u64().unwrap_or(0);
            l.active = l.running > 0;
        }
    }

    fn entry(&mut self, e: &Value) {
        match e["kind"].as_str() {
            Some("delta") => {
                let cycle = e["cycle"].as_u64().unwrap_or(self.cycle);
                let now = Instant::now();
                match self.rate {
                    Some((t, c)) if now.duration_since(t).as_millis() >= 500 => {
                        let dt = now.duration_since(t).as_secs_f64();
                        self.cycles_per_s = cycle.saturating_sub(c) as f64 / dt;
                        self.rate = Some((now, cycle));
                    }
                    None => self.rate = Some((now, cycle)),
                    _ => {}
                }
                self.cycle = cycle;
                if let Some(nodes) = e["nodes"].as_object() {
                    for (id, v) in nodes {
                        self.delta_node(id, v);
                    }
                }
            }
            Some("activity") => self.activity(&e["running"]),
            Some("event") => self.engine_event(e),
            Some("log") => self.log(e),
            Some("dropped") => self.dropped_logs += e["logs"].as_u64().unwrap_or(0),
            Some("status") => {
                self.status = e["status"].as_str().unwrap_or("running").into();
                if !e["end"].is_null() {
                    self.end = Some(e["end"].clone());
                }
                if let Some(c) = e["cycle"].as_u64() {
                    self.cycle = c;
                }
                if self.status == "stopped" {
                    for l in self.live.values_mut() {
                        l.active = false;
                        l.running = 0;
                    }
                    let why = self.end_text();
                    self.event(None, format!("run stopped: {why}"), Tone::Warn);
                }
            }
            _ => {}
        }
    }

    /// The end reason in words.
    pub fn end_text(&self) -> String {
        let Some(end) = &self.end else {
            return String::new();
        };
        match end["reason"].as_str().unwrap_or_default() {
            "source-exhausted" => {
                format!("source `{}` exhausted", end["node"].as_str().unwrap_or("?"))
            }
            "node-failed" => format!(
                "node `{}` failed at cycle {}: {}",
                end["node"].as_str().unwrap_or("?"),
                end["cycle"],
                end["error"]["message"].as_str().unwrap_or("")
            ),
            "start-failed" => format!(
                "node `{}` failed to start: {}",
                end["node"].as_str().unwrap_or("?"),
                end["error"]["message"].as_str().unwrap_or("")
            ),
            "cycle-limit" => format!("cycle limit {}", end["limit"]),
            "cancelled" => "stopped by request".into(),
            "loop-completed" => format!("loop completed ({} cycles)", end["count"]),
            other => other.to_string(),
        }
    }

    fn log(&mut self, l: &Value) {
        let line = LogLine {
            node: l["node"].as_str().map(str::to_string),
            level: l["level"].as_str().unwrap_or("info").into(),
            message: clean(l["message"].as_str().unwrap_or_default()),
        };
        if let Some(n) = &line.node {
            let q = self.node_logs.entry(n.clone()).or_default();
            q.push_back(format!("[{}] {}", line.level, line.message));
            while q.len() > NODE_LOGS {
                q.pop_front();
            }
        }
        self.logs.push_back(line);
        while self.logs.len() > LOGS {
            self.logs.pop_front();
        }
    }

    fn event(&mut self, node: Option<String>, text: String, tone: Tone) {
        let text = clean(&text);
        self.events.push_back(EventLine { node, text, tone });
        while self.events.len() > EVENTS {
            self.events.pop_front();
        }
    }

    fn engine_event(&mut self, e: &Value) {
        let d = &e["detail"];
        let node = d["node"].as_str().map(str::to_string);
        let kind = d["kind"].as_str().unwrap_or_default();
        let (text, tone) = match e["event"].as_str() {
            Some("replacement") => {
                let tone = match kind {
                    "promoted" | "committed" => Tone::Good,
                    "rolled-back" => Tone::Warn,
                    "rejected" => Tone::Bad,
                    _ => Tone::Info,
                };
                let versions = match (d["fromVersion"].as_str(), d["toVersion"].as_str()) {
                    (Some(f), Some(t)) if !f.is_empty() => format!(" {f} -> {t}"),
                    (_, Some(t)) => format!(" -> {t}"),
                    _ => String::new(),
                };
                let code = d["code"]
                    .as_str()
                    .map(|c| format!(" ({c})"))
                    .unwrap_or_default();
                (format!("replace {kind}{versions}{code}"), tone)
            }
            Some("recovery") => {
                let tone = match kind {
                    "checkpointed" | "restored" => Tone::Info,
                    "dropped" | "skipped" => Tone::Warn,
                    _ => Tone::Bad,
                };
                let msg = d["message"]
                    .as_str()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default();
                (format!("{kind} at cycle {}{msg}", d["cycle"]), tone)
            }
            Some("live") => {
                let field = d["field"]
                    .as_str()
                    .map(|f| format!(" {f}"))
                    .unwrap_or_default();
                let persisted = if d["persisted"].as_bool() == Some(true) {
                    " (persisted)"
                } else {
                    ""
                };
                (format!("{kind}{field}{persisted}"), Tone::Info)
            }
            _ => (e.to_string(), Tone::Info),
        };
        self.event(node, text, tone);
    }

    /// A command reply from the runtime.
    pub fn command_reply(&mut self, id: &str, reply: Result<Value, String>) {
        // `run` + command name for pause/resume; `node.field = value` for edits.
        let what = self.sent.remove(id).map(|(n, f, v)| {
            let label = if n == "run" {
                f.clone()
            } else {
                format!("{n}.{f} = {v}")
            };
            (label, Some((n, f, v)))
        });
        let label = what
            .as_ref()
            .map(|w| w.0.clone())
            .unwrap_or_else(|| id.to_string());
        let (text, tone) = match reply {
            Err(e) => (format!("{label}: not sent ({e})"), Tone::Bad),
            Ok(r) => match r["status"].as_str() {
                Some("applied") => {
                    if let Some((_, Some((n, f, v)))) = &what
                        && n != "run"
                    {
                        self.applied.insert((n.clone(), f.clone()), v.clone());
                    }
                    let kind = r["event"]["kind"].as_str().unwrap_or("applied");
                    let persisted = if r["event"]["persisted"].as_bool() == Some(true) {
                        ", persisted"
                    } else {
                        ""
                    };
                    (format!("{label}: applied ({kind}{persisted})"), Tone::Good)
                }
                Some("rejected") => (
                    format!(
                        "{label}: rejected ({})",
                        r["event"]["message"]
                            .as_str()
                            .or(r["event"]["code"].as_str())
                            .or(r["message"].as_str())
                            .unwrap_or("refused by the engine")
                    ),
                    Tone::Bad,
                ),
                Some("pending") => (
                    format!("{label}: pending (no outcome yet; watch the events)"),
                    Tone::Warn,
                ),
                Some("refused") => (
                    format!(
                        "{label}: refused ({}{})",
                        r["code"].as_str().unwrap_or("refused"),
                        r["message"]
                            .as_str()
                            .map(|m| format!(": {m}"))
                            .unwrap_or_default()
                    ),
                    Tone::Bad,
                ),
                _ => (format!("{label}: {r}"), Tone::Info),
            },
        };
        self.notice = Some((text, tone));
    }

    /// A preview message (`op: preview`).
    pub fn preview_message(&mut self, m: &Value) {
        if m["format"] == "png" {
            let decoded = m["data"]
                .as_str()
                .and_then(mvf_management::preview::unbase64)
                .ok_or_else(|| "bad base64".to_string())
                .and_then(|b| super::image::decode_png(&b));
            match decoded {
                Ok(img) => {
                    self.preview.image = Some((m["cycle"].as_u64().unwrap_or(0), img));
                    self.preview.note = m["encoding"]
                        .as_str()
                        .map(|e| format!("decoded from a {e}-encoded blob"));
                }
                Err(e) => self.preview.note = Some(format!("cannot show this preview: {e}")),
            }
        } else if let Some(reason) = m["reason"].as_str() {
            self.preview.image = None;
            self.preview.note = Some(format!("no image: {reason}"));
        }
    }

    /// The preview stream ended or was refused.
    pub fn preview_error(&mut self, e: &str) {
        self.preview.image = None;
        self.preview.note = Some(e.to_string());
    }

    fn id(&mut self) -> String {
        self.next_id += 1;
        format!("tui-{}-{}", self.session, self.next_id)
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.field = 0;
        self.pinned = false;
    }

    /// Follows the active node, when following.
    pub fn track(&mut self) {
        if !self.follow || self.view != View::Graph {
            return;
        }
        if let Some(i) = self.layout.order.iter().copied().find(|&i| {
            self.nodes
                .get(i)
                .is_some_and(|n| self.live.get(&n.id).is_some_and(|l| l.active))
        }) {
            self.selected = Some(i);
            self.pinned = false;
        }
    }

    fn step(&mut self, by: i32) {
        let order = &self.layout.order;
        if order.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| order.iter().position(|&n| n == s))
            .unwrap_or(0) as i32;
        let next = (at + by).clamp(0, order.len() as i32 - 1) as usize;
        let n = order[next];
        self.select(n);
    }

    /// Handles one key.
    pub fn key(&mut self, k: KeyEvent) -> Action {
        if k.kind == KeyEventKind::Release {
            return Action::None;
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        if let Some(text) = self.editing.as_mut() {
            match k.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Enter => {
                    let text = self.editing.take().unwrap_or_default();
                    return self.submit(&text);
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            }
            return Action::None;
        }
        if self.help {
            self.help = false;
            return Action::None;
        }
        match (self.view, k.code) {
            (_, KeyCode::Char('?')) => self.help = true,
            (_, KeyCode::Char('q')) => return Action::Quit,
            (_, KeyCode::Char(' ')) => return self.pause(),
            (View::Graph, KeyCode::Right | KeyCode::Tab)
                if !k.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.follow = false;
                self.step(1);
            }
            (View::Graph, KeyCode::Left | KeyCode::BackTab)
                if !k.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.follow = false;
                self.step(-1);
            }
            (View::Graph, KeyCode::Up | KeyCode::Down)
                if !k.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.follow = false;
                let by = if k.code == KeyCode::Up { -1 } else { 1 };
                if let Some(n) = self.selected.and_then(|s| self.layout.vertical(s, by)) {
                    self.select(n);
                }
            }
            (View::Graph, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down) => {
                // Shift+arrows pan the canvas.
                let (x, y) = self.scroll;
                self.scroll = match k.code {
                    KeyCode::Left => (x.saturating_sub(8), y),
                    KeyCode::Right => (x.saturating_add(8), y),
                    KeyCode::Up => (x, y.saturating_sub(3)),
                    _ => (x, y.saturating_add(3)),
                };
                self.pinned = true;
                self.follow = false;
            }
            (View::Graph, KeyCode::Char('h')) => {
                return self.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
            }
            (View::Graph, KeyCode::Char('l')) => {
                return self.key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
            }
            (View::Graph, KeyCode::Char('k')) => {
                return self.key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
            }
            (View::Graph, KeyCode::Char('j')) => {
                return self.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
            }
            (View::Graph, KeyCode::Char('f')) => {
                self.follow = !self.follow;
                self.pinned = false;
            }
            (View::Graph, KeyCode::Enter) if self.selected.is_some() => {
                self.view = View::Detail;
                self.field = 0;
            }
            (View::Detail, KeyCode::Esc | KeyCode::Backspace | KeyCode::Left) => {
                self.view = View::Graph;
                if self.preview.node.is_some() {
                    self.preview = PreviewView::default();
                    return Action::Preview(None);
                }
            }
            (View::Detail, KeyCode::Up) => self.field = self.field.saturating_sub(1),
            (View::Detail, KeyCode::Down | KeyCode::Tab) => {
                let n = self.selected_node().map_or(0, |n| n.fields.len());
                if self.field + 1 < n {
                    self.field += 1;
                }
            }
            (View::Detail, KeyCode::Enter | KeyCode::Char('e')) => {
                if !self.commands {
                    self.notice = Some((
                        "this session may not send commands (operator role needed)".into(),
                        Tone::Bad,
                    ));
                } else if self
                    .selected_node()
                    .is_some_and(|n| n.fields.get(self.field).is_some())
                {
                    self.editing = Some(String::new());
                } else {
                    self.notice =
                        Some(("this node has no live-editable fields".into(), Tone::Info));
                }
            }
            (View::Detail, KeyCode::Char('p')) => {
                let id = self.selected_node().map(|n| n.id.clone());
                if self.preview.node.is_some() {
                    self.preview = PreviewView::default();
                    return Action::Preview(None);
                }
                self.preview = PreviewView {
                    node: id.clone(),
                    image: None,
                    note: Some("waiting for a preview...".into()),
                };
                return Action::Preview(id);
            }
            _ => {}
        }
        Action::None
    }

    /// The selected node.
    pub fn selected_node(&self) -> Option<&Node> {
        self.selected.and_then(|i| self.nodes.get(i))
    }

    fn pause(&mut self) -> Action {
        if !self.commands {
            self.notice = Some(("this session may not send commands".into(), Tone::Bad));
            return Action::None;
        }
        let command = if self.status == "paused" {
            "resume"
        } else {
            "pause"
        };
        let id = self.id();
        self.sent
            .insert(id.clone(), ("run".into(), command.into(), String::new()));
        Action::Command {
            id,
            command: command.into(),
            args: json!({}),
        }
    }

    /// Validates `text` for the selected field and turns it into a command.
    fn submit(&mut self, text: &str) -> Action {
        let Some(node) = self.selected_node().cloned() else {
            return Action::None;
        };
        let Some(field) = node.fields.get(self.field).cloned() else {
            return Action::None;
        };
        let value = match parse_value(&field, text.trim()) {
            Ok(v) => v,
            Err(e) => {
                self.notice = Some((format!("{}.{}: {e}", node.id, field.field), Tone::Bad));
                return Action::None;
            }
        };
        let id = self.id();
        self.sent.insert(
            id.clone(),
            (node.id.clone(), field.field.clone(), value.to_string()),
        );
        let args = if field.command == "set-value" {
            json!({"node": node.id, "value": value})
        } else {
            json!({"node": node.id, "field": field.field, "value": value})
        };
        self.notice = Some((
            format!("{}.{} = {value}: sent", node.id, field.field),
            Tone::Info,
        ));
        Action::Command {
            id,
            command: field.command.clone(),
            args,
        }
    }
}

/// Parses an operator's text as the field's type and checks the declared schema bounds and
/// choices. The engine validates again; this only catches typos before they are sent.
pub fn parse_value(field: &Field, text: &str) -> Result<Value, String> {
    let one = |t: &str| -> Result<Value, String> {
        let v = match field.elem.as_str() {
            "int" => t
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| format!("`{t}` is not an integer"))?,
            "number" => t
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(Value::from)
                .ok_or_else(|| format!("`{t}` is not a number"))?,
            "bool" => match t.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Value::Bool(true),
                "false" | "no" | "off" | "0" => Value::Bool(false),
                _ => return Err(format!("`{t}` is not true/false")),
            },
            "string" => Value::String(t.to_string()),
            _ => serde_json::from_str(t).map_err(|e| format!("not JSON: {e}"))?,
        };
        let item_schema = field.schema.as_ref();
        if let Some(s) = item_schema {
            if let Some(choices) = s["enum"].as_array()
                && !choices.contains(&v)
            {
                return Err(format!(
                    "must be one of {}",
                    choices
                        .iter()
                        .map(Value::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(x) = v.as_f64() {
                if let Some(min) = s["minimum"].as_f64()
                    && x < min
                {
                    return Err(format!("below the minimum {min}"));
                }
                if let Some(max) = s["maximum"].as_f64()
                    && x > max
                {
                    return Err(format!("above the maximum {max}"));
                }
            }
        }
        Ok(v)
    };
    if field.shape == "list" {
        let parts: Result<Vec<Value>, String> = if text.starts_with('[') {
            serde_json::from_str::<Vec<Value>>(text)
                .map_err(|e| format!("not a JSON list: {e}"))?
                .iter()
                .map(|v| match v {
                    Value::String(s) if field.elem != "string" => one(s),
                    other => one(other.to_string().trim_matches('"')),
                })
                .collect()
        } else {
            text.split(',').map(|p| one(p.trim())).collect()
        };
        parts.map(Value::Array)
    } else {
        one(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(elem: &str, schema: Option<Value>) -> Field {
        Field {
            field: "f".into(),
            command: "configure".into(),
            elem: elem.into(),
            shape: "one".into(),
            schema,
            binding: None,
        }
    }

    #[test]
    fn typed_input_is_checked_before_it_is_sent() {
        let bounded = field("int", Some(json!({"minimum": 0, "maximum": 255})));
        assert_eq!(parse_value(&bounded, "41"), Ok(json!(41)));
        assert!(
            parse_value(&bounded, "4.5")
                .unwrap_err()
                .contains("integer")
        );
        assert!(
            parse_value(&bounded, "300")
                .unwrap_err()
                .contains("maximum")
        );
        assert!(parse_value(&bounded, "-1").unwrap_err().contains("minimum"));
        let choice = field("string", Some(json!({"enum": ["gige", "usb"]})));
        assert_eq!(parse_value(&choice, "usb"), Ok(json!("usb")));
        assert!(
            parse_value(&choice, "serial")
                .unwrap_err()
                .contains("one of")
        );
        assert_eq!(parse_value(&field("bool", None), "on"), Ok(json!(true)));
        assert_eq!(parse_value(&field("number", None), "2.5"), Ok(json!(2.5)));
        assert!(parse_value(&field("number", None), "NaN").is_err());
        let mut list = field("int", None);
        list.shape = "list".into();
        assert_eq!(parse_value(&list, "1, 2,3"), Ok(json!([1, 2, 3])));
        assert_eq!(parse_value(&list, "[4,5]"), Ok(json!([4, 5])));
    }

    fn snapshot() -> Value {
        json!({"op": "snapshot", "seq": 3, "snapshot": {
            "runtime": {"pipeline": "p", "mode": "serial", "pid": 1, "startedAtMs": 0},
            "status": "running", "cycle": 7,
            "topology": {"pipeline": "p",
                "nodes": [
                    {"id": "cam", "contract": "mvf.synthetic-source@1.0.0", "backend": "in-process", "parallelism": 1, "inputs": [], "outputs": [{"name": "frame", "type": "frame", "channel": "data"}]},
                    {"id": "thr", "contract": "mvf.primitive.value@1.0.0", "primitive": "value", "backend": "in-process", "parallelism": 1, "inputs": [], "outputs": [{"name": "value", "type": "int", "channel": "data"}],
                     "live": [{"field": "value", "command": "set-value", "type": "int", "shape": "one", "schema": {"minimum": 0, "maximum": 255}, "binding": "brightness.threshold"}]},
                    {"id": "sink", "contract": "mvf.record@1.0.0", "backend": "in-process", "parallelism": 1, "inputs": [{"name": "frame", "type": "frame", "channel": "data"}], "outputs": []}
                ],
                "edges": {"data": [{"from": "cam.frame", "to": "sink.frame", "type": "frame"}], "control": []}},
            "nodes": {"cam": {"executed": 7, "latencyUs": {"p50": 3.0}, "window": {"executed": 7, "meanUs": 3.0}}},
            "activity": {"cam": 0},
            "recentLogs": [{"node": "cam", "level": "info", "message": "hello"}],
            "recentEvents": []
        }})
    }

    #[test]
    fn a_snapshot_then_entries_build_the_model_and_a_gap_snapshot_replaces_it() {
        let mut app = App::new("t");
        app.apply(&snapshot());
        assert_eq!(app.nodes.len(), 3);
        assert_eq!(app.layout.order.len(), 3);
        assert_eq!(app.live_of("cam").executed, 7);
        assert_eq!(app.node_logs["cam"].len(), 1);
        assert_eq!(app.nodes[0].category(), "source");
        assert_eq!(app.nodes[1].category(), "value");
        assert_eq!(app.nodes[2].category(), "sink");
        app.apply(&json!({"op": "entry", "seq": 4, "entry": {"kind": "delta", "cycle": 9, "nodes": {"sink": {"executed": 9, "failures": 2, "restarts": 1, "contract": "mvf.record@2.0.0", "backend": "worker", "window": {"executed": 2, "meanUs": 40.0}}}}}));
        app.apply(&json!({"op": "entry", "seq": 5, "entry": {"kind": "activity", "running": {"sink": 1}}}));
        let sink = app.live_of("sink");
        assert!(sink.active && sink.failures == 2 && sink.restarts == 1);
        assert_eq!(
            (sink.contract.as_deref(), sink.backend.as_deref()),
            (Some("mvf.record@2.0.0"), Some("worker"))
        );
        app.follow = true;
        app.track();
        assert_eq!(
            app.selected_node().map(|n| n.id.as_str()),
            Some("sink"),
            "follows the active node"
        );
        app.apply(&json!({"op": "entry", "seq": 5, "entry": {"kind": "event", "event": "replacement", "detail": {"node": "sink", "kind": "rolled-back", "fromVersion": "mvf.record@2.0.0", "toVersion": "mvf.record@1.0.0", "code": "replace.candidate-failed"}}}));
        assert!(
            app.events
                .back()
                .is_some_and(|e| e.tone == Tone::Warn && e.text.contains("rolled-back"))
        );
        app.apply(&json!({"op": "entry", "seq": 6, "entry": {"kind": "status", "status": "stopped", "end": {"reason": "source-exhausted", "node": "cam"}}}));
        assert_eq!(app.end_node(), Some(("cam".into(), "exhausted")));
        assert!(!app.live_of("sink").active, "nothing runs after the end");
        // A gap: the next snapshot replaces everything.
        app.apply(&json!({"op": "gap", "from": 7, "to": 900}));
        app.apply(&snapshot());
        assert_eq!(app.gaps, 1);
        assert_eq!(app.live_of("sink").executed, 0);
        assert_eq!(app.status, "running");
    }

    #[test]
    fn escape_sequences_in_logs_never_reach_the_terminal() {
        let mut app = App::new("t");
        app.apply(&json!({"op": "entry", "seq": 1, "entry": {"kind": "log", "node": "cam", "level": "info", "message": "bad \u{1b}[2J\u{7} line\r\n"}}));
        let line = &app.logs.back().expect("line").message;
        assert!(!line.chars().any(char::is_control), "{line:?}");
        assert!(line.contains("bad") && line.contains("line"));
    }

    #[test]
    fn memory_stays_bounded_whatever_the_runtime_sends() {
        let mut app = App::new("t");
        app.apply(&snapshot());
        for i in 0..5000 {
            app.apply(&json!({"op": "entry", "seq": 10 + i, "entry": {"kind": "log", "node": "cam", "level": "info", "message": format!("line {i}")}}));
            app.apply(&json!({"op": "entry", "seq": 10 + i, "entry": {"kind": "event", "event": "recovery", "detail": {"node": "cam", "kind": "skipped", "cycle": i}}}));
            app.apply(&json!({"op": "entry", "seq": 10 + i, "entry": {"kind": "delta", "cycle": i, "nodes": {"cam": {"executed": i, "window": {"executed": 1, "meanUs": 5.0}}}}}));
        }
        assert_eq!(app.logs.len(), LOGS);
        assert_eq!(app.node_logs["cam"].len(), NODE_LOGS);
        assert_eq!(app.events.len(), EVENTS);
        assert_eq!(app.live_of("cam").history.len(), HISTORY);
    }

    #[test]
    fn keys_navigate_edit_and_command_through_the_management_api() {
        let mut app = App::new("t");
        app.apply(&snapshot());
        app.commands = true;
        let key = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
        // cam, thr in layer 0; sink in layer 1 (reading order).
        assert_eq!(app.selected_node().map(|n| n.id.as_str()), Some("cam"));
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected_node().map(|n| n.id.as_str()), Some("thr"));
        assert!(!app.follow, "a manual move stops following");
        app.key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Detail);
        app.key(key(KeyCode::Enter));
        for c in "300".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Action::None,
            "out of range: not sent"
        );
        assert!(app.notice.as_ref().is_some_and(|n| n.0.contains("maximum")));
        app.key(key(KeyCode::Enter));
        for c in "41".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        let Action::Command { id, command, args } = app.key(key(KeyCode::Enter)) else {
            panic!("a command");
        };
        assert_eq!(
            (command.as_str(), &args),
            ("set-value", &json!({"node": "thr", "value": 41}))
        );
        app.command_reply(
            &id,
            Ok(json!({"status": "applied", "event": {"kind": "value-set", "persisted": true}})),
        );
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.1 == Tone::Good && n.0.contains("persisted"))
        );
        assert_eq!(
            app.applied
                .get(&("thr".into(), "value".into()))
                .map(String::as_str),
            Some("41")
        );
        // Space pauses (a command), q quits, ctrl+c quits.
        assert!(
            matches!(app.key(key(KeyCode::Char(' '))), Action::Command { command, .. } if command == "pause")
        );
        assert_eq!(
            app.key(key(KeyCode::Char('p'))),
            Action::Preview(Some("thr".into()))
        );
        assert_eq!(app.key(key(KeyCode::Esc)), Action::Preview(None));
        assert_eq!(app.key(key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        // An observer session cannot edit.
        app.commands = false;
        app.view = View::Detail;
        app.key(key(KeyCode::Enter));
        assert!(app.editing.is_none());
    }
}
