//! The host's state, without windows: what the windowed host and `--headless` share.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvf_display::registry::{Discovered, discover};
use mvf_display::surface::{FLAG_ATTACHED, FLAG_CLOSED, FLAG_FOCUSED, FLAG_VISIBLE};
use mvf_display::{Descriptor, FrameInfo, InputEvent, InputKind, SurfaceError, SurfaceReader};

use crate::router::admits;

/// What happened to an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// Queued for the node.
    Delivered,
    /// Not granted by the node's contract.
    Denied,
    /// The node has not consumed its ring (input never waits for a slow node).
    Dropped,
    /// No such surface.
    Unknown,
}

/// One surface the host presents.
#[derive(Debug)]
pub struct Shown {
    /// Its descriptor.
    pub descriptor: Descriptor,
    /// The reader (holds the host lock).
    pub reader: SurfaceReader,
    /// The latest verified frame.
    pub frame: Vec<u8>,
    /// Its description.
    pub info: Option<FrameInfo>,
    /// The user closed the window.
    pub closed: bool,
    /// Frames taken.
    pub frames: u64,
    /// Present → host copy latency samples (µs), bounded.
    pub latency_us: Vec<u64>,
    /// Input routed / denied / dropped.
    pub routed: [u64; 3],
}

/// Host state.
#[derive(Debug)]
pub struct HostCore {
    root: PathBuf,
    max_windows: usize,
    surfaces: BTreeMap<PathBuf, Shown>,
    last_scan: Option<Instant>,
    last_beat: Option<Instant>,
    /// Lines for the operator (stderr).
    pub log: Vec<String>,
    /// Surfaces another host already presents.
    pub busy: u64,
}

/// Surfaces added and removed by a scan.
#[derive(Debug, Default)]
pub struct ScanDelta {
    /// Newly presented.
    pub added: Vec<PathBuf>,
    /// Gone (engine stopped or died).
    pub removed: Vec<PathBuf>,
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

impl HostCore {
    /// A host over the rendezvous directory `root`, presenting at most `max_windows` surfaces.
    pub fn new(root: impl Into<PathBuf>, max_windows: usize) -> Self {
        Self {
            root: root.into(),
            max_windows,
            surfaces: BTreeMap::new(),
            last_scan: None,
            last_beat: None,
            log: Vec::new(),
            busy: 0,
        }
    }

    /// The rendezvous directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Discovers surfaces (at most every 250 ms unless `force`).
    pub fn scan(&mut self, force: bool) -> ScanDelta {
        let mut delta = ScanDelta::default();
        if !force
            && self
                .last_scan
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return delta;
        }
        self.last_scan = Some(Instant::now());
        let found: BTreeMap<PathBuf, Discovered> = discover(&self.root)
            .into_iter()
            .map(|d| (d.surface.clone(), d))
            .collect();
        let gone: Vec<PathBuf> = self
            .surfaces
            .keys()
            .filter(|p| !found.contains_key(*p))
            .cloned()
            .collect();
        for path in gone {
            if let Some(s) = self.surfaces.remove(&path) {
                self.log.push(format!(
                    "surface gone: {} [{}] after {} frames",
                    s.descriptor.title, s.descriptor.node, s.frames
                ));
            }
            delta.removed.push(path);
        }
        for (path, d) in found {
            if self.surfaces.contains_key(&path) || self.surfaces.len() >= self.max_windows {
                continue;
            }
            match SurfaceReader::open(&path) {
                Ok(reader) => {
                    self.log.push(format!(
                        "presenting {} [{} #{}] input: keyboard={} mouse={}",
                        d.descriptor.title,
                        d.descriptor.node,
                        d.descriptor.index,
                        d.descriptor.grants.keyboard,
                        d.descriptor.grants.mouse
                    ));
                    self.surfaces.insert(
                        path.clone(),
                        Shown {
                            descriptor: d.descriptor,
                            reader,
                            frame: Vec::new(),
                            info: None,
                            closed: false,
                            frames: 0,
                            latency_us: Vec::new(),
                            routed: [0; 3],
                        },
                    );
                    delta.added.push(path);
                }
                Err(SurfaceError::Busy(_)) => self.busy += 1,
                // Not yet complete, or not a valid surface: never presented, retried next scan.
                Err(e) => self.log.push(format!("skipping {}: {e}", path.display())),
            }
        }
        delta
    }

    /// Takes the newest verified frame of every open surface; returns those with a new frame.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut fresh = Vec::new();
        for (path, s) in &mut self.surfaces {
            if s.closed {
                continue;
            }
            if let Some(info) = s.reader.take_latest(&mut s.frame) {
                s.frames += 1;
                if s.latency_us.len() < 100_000 {
                    s.latency_us
                        .push(now_us().saturating_sub(info.timestamp_us));
                }
                s.info = Some(info);
                fresh.push(path.clone());
            }
        }
        fresh
    }

    /// Refreshes every surface's heartbeat (at most every 250 ms unless `force`).
    pub fn heartbeat(&mut self, force: bool) {
        if !force
            && self
                .last_beat
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.last_beat = Some(Instant::now());
        for s in self.surfaces.values_mut() {
            s.reader.update_status(|_| {});
        }
    }

    /// The surface at `path`.
    pub fn get(&self, path: &Path) -> Option<&Shown> {
        self.surfaces.get(path)
    }

    /// Open surfaces.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.surfaces.keys().cloned().collect()
    }

    /// Changes a surface's status (requested size, scale, focus, visibility).
    pub fn status(&mut self, path: &Path, change: impl FnOnce(&mut mvf_display::HostStatus)) {
        if let Some(s) = self.surfaces.get_mut(path) {
            s.reader.update_status(change);
        }
    }

    /// Marks focus.
    pub fn focus(&mut self, path: &Path, focused: bool) {
        self.status(path, |s| {
            if focused {
                s.flags |= FLAG_FOCUSED;
            } else {
                s.flags &= !FLAG_FOCUSED;
            }
        });
        let mut e = InputEvent::new(InputKind::Focus);
        e.code = u32::from(focused);
        self.route(path, &e);
    }

    /// Marks visibility.
    pub fn visible(&mut self, path: &Path, visible: bool) {
        self.status(path, |s| {
            if visible {
                s.flags |= FLAG_VISIBLE;
            } else {
                s.flags &= !FLAG_VISIBLE;
            }
        });
    }

    /// Routes an event if the node's grants admit it.
    pub fn route(&mut self, path: &Path, event: &InputEvent) -> Routed {
        let Some(s) = self.surfaces.get_mut(path) else {
            return Routed::Unknown;
        };
        if !admits(event.kind, s.descriptor.grants) {
            s.routed[1] += 1;
            return Routed::Denied;
        }
        if s.reader.push(event) {
            s.routed[0] += 1;
            Routed::Delivered
        } else {
            s.routed[2] += 1;
            Routed::Dropped
        }
    }

    /// The user closed the window: the node is told, the host stops presenting the surface but
    /// keeps it (so no other host grabs it) until the node's engine goes away.
    pub fn close(&mut self, path: &Path) {
        self.route(path, &InputEvent::new(InputKind::CloseRequested));
        if let Some(s) = self.surfaces.get_mut(path) {
            s.closed = true;
            s.reader.update_status(|st| {
                st.flags = (st.flags | FLAG_CLOSED | FLAG_ATTACHED) & !(FLAG_FOCUSED | FLAG_VISIBLE)
            });
            self.log.push(format!(
                "window closed by the user: {} [{}]",
                s.descriptor.title, s.descriptor.node
            ));
        }
    }

    /// Statistics for tests and operators.
    pub fn stats(&self) -> serde_json::Value {
        let surfaces: Vec<serde_json::Value> = self
            .surfaces
            .values()
            .map(|s| {
                let mut l = s.latency_us.clone();
                l.sort_unstable();
                let pct = |p: f64| {
                    if l.is_empty() {
                        0.0
                    } else {
                        l[((l.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0
                    }
                };
                serde_json::json!({
                    "node": s.descriptor.node,
                    "index": s.descriptor.index,
                    "title": s.descriptor.title,
                    "frames": s.frames,
                    "lastSeq": s.info.map(|i| i.seq),
                    "size": s.info.map(|i| [i.width, i.height]),
                    "rejected": s.reader.rejected,
                    "closed": s.closed,
                    "latencyMs": {"p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99)},
                    "input": {"delivered": s.routed[0], "denied": s.routed[1], "dropped": s.routed[2]},
                    "status": {
                        "requested": [s.reader.status().requested_width, s.reader.status().requested_height],
                        "scaleMilli": s.reader.status().scale_milli,
                    },
                })
            })
            .collect();
        serde_json::json!({ "pid": std::process::id(), "busy": self.busy, "surfaces": surfaces })
    }
}
