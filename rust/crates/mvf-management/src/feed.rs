//! The observation feed: one ordered, sequenced stream of entries (node deltas, engine events,
//! logs, status) in a bounded ring, plus the current absolute state for snapshots.
//!
//! **The engine never waits for it.** The [`Tap`] runs on the engine thread. It accumulates per-node
//! counters locally and publishes them every `publish_every` with `try_lock`; when a reader holds
//! the lock it keeps accumulating and tries again at the next cycle. Readers copy reference-counted,
//! pre-serialised lines under the lock and write them to their sockets outside it. A reader that
//! falls behind the ring is told how many entries it missed (a *gap*) and gets a fresh snapshot.
//! Nothing waits for a client.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use mvf_engine::{
    EndReason, EngineEvents, LiveEventKind, LogEvent, NodeOutcome, Observer, RunReport,
};
use serde_json::{Value, json};

use crate::model::{API, Histogram, Topology};
use crate::preview::{PreviewConfig, PreviewHub, Sampler};

/// Feed sizing and budgets.
#[derive(Debug, Clone)]
pub struct FeedConfig {
    /// Entries kept for subscribers that fall behind (default 4096).
    pub capacity: usize,
    /// How often node counters are published (default 100 ms).
    pub publish_every: Duration,
    /// Log lines admitted per second, sustained (default 200); excess is counted, not queued.
    pub logs_per_second: f64,
    /// Log lines admitted in a burst (default 1000).
    pub log_burst: f64,
    /// Recent log lines and events kept in snapshots (default 50 each).
    pub recent: usize,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            capacity: 4096,
            publish_every: Duration::from_millis(100),
            logs_per_second: 200.0,
            log_burst: 1000.0,
            recent: 50,
        }
    }
}

/// The runtime identity a client sees in `hello` and snapshots.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// Pipeline name.
    pub pipeline: String,
    /// Runtime version.
    pub version: String,
    /// `serial` or `pipelined`.
    pub mode: String,
}

/// What a reader gets after its cursor.
#[derive(Debug)]
pub enum Read {
    /// Lines with `seq > cursor`, in order (pre-serialised `{"op":"entry",…}` JSON).
    Entries(Vec<(u64, Arc<str>)>),
    /// The reader's cursor fell behind the ring: entries `from..=to` are gone.
    Gap {
        /// First missed sequence.
        from: u64,
        /// Last missed sequence.
        to: u64,
    },
}

#[derive(Debug, Default)]
struct State {
    status: &'static str,
    end: Option<Value>,
    cycle: u64,
    nodes: BTreeMap<String, Value>,
    recent_logs: VecDeque<Value>,
    recent_events: VecDeque<Value>,
}

#[derive(Debug)]
struct Shared {
    entries: VecDeque<(u64, Arc<str>)>,
    /// Sequence of the last entry published (0 = none yet).
    last: u64,
    state: State,
    topology: Arc<Value>,
    tokens: f64,
    refilled: Instant,
    dropped_logs: u64,
}

/// The observation feed of one running engine.
#[derive(Debug)]
pub struct Feed {
    shared: Mutex<Shared>,
    changed: Condvar,
    config: FeedConfig,
    runtime: RuntimeInfo,
    started: Instant,
    started_ms: u64,
    /// Publications the engine thread deferred because a reader held the lock.
    deferred: AtomicU64,
    /// Previews, when the operator opted nodes in.
    previews: OnceLock<Arc<PreviewHub>>,
}

impl Feed {
    /// A feed for a pipeline whose topology is filled in later ([`Feed::set_topology`]).
    pub fn new(runtime: RuntimeInfo, config: FeedConfig) -> Arc<Self> {
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Arc::new(Self {
            shared: Mutex::new(Shared {
                entries: VecDeque::with_capacity(config.capacity.min(65536)),
                last: 0,
                state: State {
                    status: "starting",
                    ..State::default()
                },
                topology: Arc::new(Value::Null),
                tokens: config.log_burst,
                refilled: Instant::now(),
                dropped_logs: 0,
            }),
            changed: Condvar::new(),
            config,
            runtime,
            started: Instant::now(),
            started_ms,
            deferred: AtomicU64::new(0),
            previews: OnceLock::new(),
        })
    }

    /// Enables previews of `config.nodes` (before the [`Tap`] is created). Off by default.
    pub fn enable_previews(&self, config: PreviewConfig) -> Arc<PreviewHub> {
        self.previews
            .get_or_init(|| PreviewHub::start(config))
            .clone()
    }

    /// The preview hub, if previews are enabled.
    pub fn previews(&self) -> Option<&Arc<PreviewHub>> {
        self.previews.get()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panicking reader must not take observation down with it.
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets the topology (before the run starts).
    pub fn set_topology(&self, topology: &Topology) {
        let value = serde_json::to_value(topology).unwrap_or(Value::Null);
        self.lock().topology = Arc::new(value);
    }

    /// Runtime identity for `hello`.
    pub fn hello(&self) -> Value {
        let s = self.lock();
        self.hello_with(&s)
    }

    fn hello_with(&self, s: &Shared) -> Value {
        // The topology names the pipeline once the engine is built.
        let pipeline = s.topology["pipeline"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| self.runtime.pipeline.clone());
        json!({
            "pid": std::process::id(),
            "version": self.runtime.version,
            "pipeline": pipeline,
            "mode": self.runtime.mode,
            "startedAtMs": self.started_ms,
            "api": API,
        })
    }

    /// Milliseconds since the feed was created.
    fn at_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1e3
    }

    /// Publications deferred because a reader held the lock (they were retried, not lost).
    pub fn deferred(&self) -> u64 {
        self.deferred.load(Ordering::Relaxed)
    }

    /// The sequence of the last entry published.
    pub fn last_seq(&self) -> u64 {
        self.lock().last
    }

    fn push(&self, shared: &mut Shared, entry: Value) {
        shared.last += 1;
        let line = json!({"op": "entry", "seq": shared.last, "entry": entry}).to_string();
        shared.entries.push_back((shared.last, Arc::from(line)));
        while shared.entries.len() > self.config.capacity {
            shared.entries.pop_front();
        }
    }

    fn remember(&self, list: &mut VecDeque<Value>, v: Value) {
        list.push_back(v);
        while list.len() > self.config.recent {
            list.pop_front();
        }
    }

    /// Adds a log line, within the log budget. Called from any thread (the `Logs` stream); it may
    /// wait for a reader's copy of the ring, never for a client.
    pub fn log(&self, e: &LogEvent) {
        let mut s = self.lock();
        let now = Instant::now();
        let refill = now.duration_since(s.refilled).as_secs_f64() * self.config.logs_per_second;
        s.tokens = (s.tokens + refill).min(self.config.log_burst);
        s.refilled = now;
        if s.tokens < 1.0 {
            s.dropped_logs += 1;
            return;
        }
        s.tokens -= 1.0;
        if s.dropped_logs > 0 {
            let n = std::mem::take(&mut s.dropped_logs);
            self.push(&mut s, json!({"kind": "dropped", "logs": n}));
        }
        let entry = json!({
            "kind": "log",
            "atMs": self.at_ms(),
            "node": e.node,
            "generation": e.generation,
            "level": e.level,
            "message": e.message,
        });
        let mut recent = std::mem::take(&mut s.state.recent_logs);
        self.remember(&mut recent, entry.clone());
        s.state.recent_logs = recent;
        self.push(&mut s, entry);
        drop(s);
        self.changed.notify_all();
    }

    /// Publishes node views and events from the engine thread, or returns `false` if a reader
    /// holds the lock (the caller keeps its data and retries).
    fn try_publish(
        &self,
        cycle: u64,
        nodes: &[(String, Value)],
        events: &[Value],
        status: Option<&'static str>,
    ) -> bool {
        let Ok(mut s) = self.shared.try_lock() else {
            self.deferred.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        self.apply(&mut s, cycle, nodes, events, status, None);
        drop(s);
        self.changed.notify_all();
        true
    }

    fn apply(
        &self,
        s: &mut Shared,
        cycle: u64,
        nodes: &[(String, Value)],
        events: &[Value],
        status: Option<&'static str>,
        end: Option<Value>,
    ) {
        s.state.cycle = cycle;
        for e in events {
            let mut recent = std::mem::take(&mut s.state.recent_events);
            self.remember(&mut recent, e.clone());
            s.state.recent_events = recent;
            self.push(s, e.clone());
        }
        if !nodes.is_empty() {
            let mut map = serde_json::Map::new();
            for (id, v) in nodes {
                s.state.nodes.insert(id.clone(), v.clone());
                map.insert(id.clone(), v.clone());
            }
            self.push(
                s,
                json!({"kind": "delta", "atMs": self.at_ms(), "cycle": cycle, "nodes": map}),
            );
        }
        if let Some(status) = status
            && (s.state.status != status || end.is_some())
        {
            s.state.status = status;
            s.state.end = end.clone();
            let mut entry =
                json!({"kind": "status", "atMs": self.at_ms(), "status": status, "cycle": cycle});
            if let Some(end) = end {
                entry["end"] = end;
            }
            self.push(s, entry);
        }
    }

    /// The current state with the sequence it reflects: entries with a higher sequence come after.
    pub fn snapshot(&self) -> (u64, Value) {
        let s = self.lock();
        let snapshot = json!({
            "api": API,
            "runtime": self.hello_with(&s),
            "status": s.state.status,
            "end": s.state.end,
            "cycle": s.state.cycle,
            "topology": *s.topology,
            "nodes": s.state.nodes,
            "recentLogs": s.state.recent_logs,
            "recentEvents": s.state.recent_events,
            "deferredPublications": self.deferred(),
        });
        (s.last, snapshot)
    }

    /// Waits until `until` for an entry after `after` that `f` recognises (a command's outcome).
    pub fn wait_for<T>(
        &self,
        after: u64,
        until: Instant,
        mut f: impl FnMut(&Value) -> Option<T>,
    ) -> Option<T> {
        let mut cursor = after;
        loop {
            let now = Instant::now();
            if now >= until {
                return None;
            }
            match self.read_after(cursor, until - now) {
                Read::Entries(entries) => {
                    for (seq, line) in entries {
                        cursor = seq;
                        if let Ok(v) = serde_json::from_str::<Value>(&line)
                            && let Some(t) = f(&v["entry"])
                        {
                            return Some(t);
                        }
                    }
                }
                Read::Gap { to, .. } => cursor = to,
            }
        }
    }

    /// Entries after `cursor`, waiting up to `wait` for one to arrive.
    pub fn read_after(&self, cursor: u64, wait: Duration) -> Read {
        let mut s = self.lock();
        if s.last <= cursor {
            let (guard, _) = self
                .changed
                .wait_timeout_while(s, wait, |s| s.last <= cursor)
                .unwrap_or_else(|e| e.into_inner());
            s = guard;
        }
        let oldest = s.entries.front().map(|(seq, _)| *seq).unwrap_or(s.last + 1);
        if cursor + 1 < oldest && s.last > cursor {
            return Read::Gap {
                from: cursor + 1,
                to: oldest - 1,
            };
        }
        Read::Entries(
            s.entries
                .iter()
                .filter(|(seq, _)| *seq > cursor)
                .cloned()
                .collect(),
        )
    }
}

#[derive(Debug, Default)]
struct NodeAcc {
    generation: u64,
    executed: u64,
    skipped: u64,
    last_skip: Option<Value>,
    latency: Histogram,
    total_us: u64,
    window_executed: u64,
    window_us: u64,
    window_max_us: u64,
    dirty: bool,
}

/// The engine-side half of the feed: an [`Observer`] that accumulates per-node counters on the
/// engine thread and publishes them without ever blocking it.
#[derive(Debug)]
pub struct Tap {
    feed: Arc<Feed>,
    previews: Option<Arc<PreviewHub>>,
    sampler: Sampler,
    index: BTreeMap<String, usize>,
    nodes: Vec<(String, NodeAcc)>,
    pending: Vec<Value>,
    status: Option<&'static str>,
    cycle: u64,
    last_publish: Instant,
    window_started: Instant,
}

impl Tap {
    /// A tap for `feed`'s topology.
    pub fn new(feed: Arc<Feed>, node_ids: impl IntoIterator<Item = String>) -> Self {
        let nodes: Vec<_> = node_ids
            .into_iter()
            .map(|id| (id, NodeAcc::default()))
            .collect();
        Self {
            index: nodes
                .iter()
                .enumerate()
                .map(|(i, (id, _))| (id.clone(), i))
                .collect(),
            nodes,
            previews: feed.previews().cloned(),
            sampler: Sampler::default(),
            feed,
            pending: Vec::new(),
            status: Some("running"),
            cycle: 0,
            last_publish: Instant::now(),
            window_started: Instant::now(),
        }
    }

    fn view(acc: &NodeAcc, window_s: f64) -> Value {
        json!({
            "generation": acc.generation,
            "executed": acc.executed,
            "skipped": acc.skipped,
            "lastSkip": acc.last_skip,
            "latencyUs": {
                "p50": acc.latency.quantile(0.5),
                "p95": acc.latency.quantile(0.95),
                "p99": acc.latency.quantile(0.99),
                "max": acc.latency.max(),
                "mean": if acc.executed > 0 { acc.total_us as f64 / acc.executed as f64 } else { 0.0 },
            },
            "window": {
                "executed": acc.window_executed,
                "meanUs": if acc.window_executed > 0 { acc.window_us as f64 / acc.window_executed as f64 } else { 0.0 },
                "maxUs": acc.window_max_us,
                "perSecond": if window_s > 0.0 { acc.window_executed as f64 / window_s } else { 0.0 },
            },
        })
    }

    fn publish(&mut self, force: bool) {
        if !force && self.last_publish.elapsed() < self.feed.config.publish_every {
            return;
        }
        let window_s = self.window_started.elapsed().as_secs_f64();
        let nodes: Vec<(String, Value)> = self
            .nodes
            .iter()
            .filter(|(_, a)| a.dirty)
            .map(|(id, a)| (id.clone(), Self::view(a, window_s)))
            .collect();
        if nodes.is_empty() && self.pending.is_empty() && self.status.is_none() {
            self.last_publish = Instant::now();
            return;
        }
        if self
            .feed
            .try_publish(self.cycle, &nodes, &self.pending, self.status)
        {
            self.pending.clear();
            self.status = None;
            for (_, a) in &mut self.nodes {
                a.dirty = false;
                a.window_executed = 0;
                a.window_us = 0;
                a.window_max_us = 0;
            }
            self.last_publish = Instant::now();
            self.window_started = self.last_publish;
        }
    }

    /// Publishes the final state once the run is over (exact counters from the report). Waits for
    /// the lock: the run has ended, nothing is on the hot path any more.
    pub fn finish(&mut self, report: &RunReport) {
        let window_s = self.window_started.elapsed().as_secs_f64();
        for stats in &report.nodes {
            if let Some(&i) = self.index.get(&stats.id) {
                let a = &mut self.nodes[i].1;
                a.generation = stats.generation;
                a.executed = stats.executed;
                a.skipped = stats.skipped;
                a.dirty = true;
            }
        }
        let nodes: Vec<(String, Value)> = self
            .nodes
            .iter()
            .map(|(id, a)| (id.clone(), Self::view(a, window_s)))
            .collect();
        let end = serde_json::to_value(&report.end).unwrap_or(Value::Null);
        let feed = Arc::clone(&self.feed);
        let mut s = feed.lock();
        feed.apply(
            &mut s,
            report.cycles_completed,
            &nodes,
            &self.pending,
            Some("stopped"),
            Some(end),
        );
        drop(s);
        feed.changed.notify_all();
        self.pending.clear();
    }

    /// Records a run that could not start (no report yet): status `stopped` with the failure.
    pub fn failed_to_start(&mut self, reason: &EndReason) {
        let end = serde_json::to_value(reason).unwrap_or(Value::Null);
        let feed = Arc::clone(&self.feed);
        let mut s = feed.lock();
        feed.apply(&mut s, 0, &[], &[], Some("stopped"), Some(end));
        drop(s);
        feed.changed.notify_all();
    }
}

impl Observer for Tap {
    fn node(&mut self, cycle: u64, node: &str, generation: u64, outcome: &NodeOutcome) {
        let _ = cycle;
        let Some(&i) = self.index.get(node) else {
            return;
        };
        let a = &mut self.nodes[i].1;
        a.generation = generation;
        a.dirty = true;
        match outcome {
            NodeOutcome::Executed { duration, .. } => {
                let us = duration.as_micros().min(u128::from(u64::MAX)) as u64;
                a.executed += 1;
                a.total_us += us;
                a.latency.record(us);
                a.window_executed += 1;
                a.window_us += us;
                a.window_max_us = a.window_max_us.max(us);
            }
            NodeOutcome::Skipped(reason) => {
                a.skipped += 1;
                a.last_skip = serde_json::to_value(reason).ok();
            }
        }
    }

    fn cycle_completed(&mut self, cycle: u64) {
        self.cycle = cycle;
        self.publish(false);
    }

    fn events(&mut self, events: &EngineEvents<'_>) {
        for e in events.replacements {
            self.pending
                .push(json!({"kind": "event", "event": "replacement", "detail": e}));
        }
        for e in events.recoveries {
            self.pending
                .push(json!({"kind": "event", "event": "recovery", "detail": e}));
        }
        for e in events.live {
            match e.kind {
                LiveEventKind::Paused => self.status = Some("paused"),
                LiveEventKind::Resumed => self.status = Some("running"),
                _ => {}
            }
            self.pending
                .push(json!({"kind": "event", "event": "live", "detail": e}));
        }
        // Events are rare and matter: publish them now rather than at the next interval.
        self.publish(true);
    }

    fn wants_outputs(&mut self, node: &str) -> bool {
        match &self.previews {
            Some(hub) => self.sampler.wants(hub, node),
            None => false,
        }
    }

    fn outputs(&mut self, cycle: u64, node: &str, outputs: &BTreeMap<String, mvf_engine::Value>) {
        if let Some(hub) = &self.previews
            && let Some(frame) = outputs
                .values()
                .find(|v| matches!(v, mvf_engine::Value::Frame(_)))
        {
            hub.offer(node, cycle, frame);
        }
    }
}

/// Forwards every event to two observers (the CLI's own and a [`Tap`]).
#[derive(Debug)]
pub struct Tee<'a, A: ?Sized, B: ?Sized> {
    a: &'a mut A,
    b: &'a mut B,
    /// Per node, how many samples each of the two asked for and has not had yet. In the serial
    /// executor `outputs` follows `wants_outputs` at once; in the pipelined one other nodes'
    /// calls come in between (see [`Observer::wants_outputs`]).
    wanted: Vec<(String, [u32; 2])>,
}

impl<'a, A: Observer + ?Sized, B: Observer + ?Sized> Tee<'a, A, B> {
    /// Both observers, `a` first.
    pub fn new(a: &'a mut A, b: &'a mut B) -> Self {
        Self {
            a,
            b,
            wanted: Vec::new(),
        }
    }
}

impl<A: Observer + ?Sized, B: Observer + ?Sized> Observer for Tee<'_, A, B> {
    fn node(&mut self, cycle: u64, node: &str, generation: u64, outcome: &NodeOutcome) {
        self.a.node(cycle, node, generation, outcome);
        self.b.node(cycle, node, generation, outcome);
    }

    fn cycle_completed(&mut self, cycle: u64) {
        self.a.cycle_completed(cycle);
        self.b.cycle_completed(cycle);
    }

    fn events(&mut self, events: &EngineEvents<'_>) {
        self.a.events(events);
        self.b.events(events);
    }

    fn wants_outputs(&mut self, node: &str) -> bool {
        let asked = [self.a.wants_outputs(node), self.b.wants_outputs(node)];
        if !asked.contains(&true) {
            return false;
        }
        let i = match self.wanted.iter().position(|(n, _)| n == node) {
            Some(i) => i,
            None => {
                self.wanted.push((node.to_string(), [0, 0]));
                self.wanted.len() - 1
            }
        };
        for (count, yes) in self.wanted[i].1.iter_mut().zip(asked) {
            if yes {
                *count = count.saturating_add(1);
            }
        }
        true
    }

    fn outputs(&mut self, cycle: u64, node: &str, outputs: &BTreeMap<String, mvf_engine::Value>) {
        let Some((_, counts)) = self.wanted.iter_mut().find(|(n, _)| n == node) else {
            return;
        };
        let due = [counts[0] > 0, counts[1] > 0];
        for (count, d) in counts.iter_mut().zip(due) {
            if d {
                *count -= 1;
            }
        }
        if due[0] {
            self.a.outputs(cycle, node, outputs);
        }
        if due[1] {
            self.b.outputs(cycle, node, outputs);
        }
    }
}
