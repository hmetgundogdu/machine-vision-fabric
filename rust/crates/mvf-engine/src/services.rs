//! Host services shared by every backend: sink records, structured node logs and the engine-owned
//! shared-memory arena (created lazily, so a run without workers pays nothing).

use std::sync::{Arc, Mutex, OnceLock};

use mvf_memory::{Arena, ArenaConfig, ArenaError};
use serde::Serialize;

use crate::builtins::Recorder;

/// One structured log line from a node (worker `log` message or forwarded stderr).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEvent {
    /// Node id.
    pub node: String,
    /// Generation of the node that logged.
    pub generation: u64,
    /// `debug` | `info` | `warn` | `error` | `stderr` (forwarded) | `supervisor` (host lifecycle).
    pub level: String,
    /// The message.
    pub message: String,
}

type LogStream = Arc<dyn Fn(&LogEvent) + Send + Sync>;

/// Thread-safe log store, optionally streamed as events arrive.
#[derive(Clone, Default)]
pub struct Logs {
    events: Arc<Mutex<Vec<LogEvent>>>,
    stream: Option<LogStream>,
}

impl std::fmt::Debug for Logs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logs").field("events", &self.len()).finish()
    }
}

/// At most this many events are retained in memory (oldest dropped first).
const MAX_RETAINED_LOGS: usize = 10_000;

impl Logs {
    /// A store that also calls `stream` for every event.
    pub fn streaming(stream: impl Fn(&LogEvent) + Send + Sync + 'static) -> Self {
        Self {
            events: Arc::default(),
            stream: Some(Arc::new(stream)),
        }
    }

    /// Appends an event.
    pub fn push(&self, event: LogEvent) {
        if let Some(stream) = &self.stream {
            stream(&event);
        }
        if let Ok(mut e) = self.events.lock() {
            if e.len() >= MAX_RETAINED_LOGS {
                e.remove(0);
            }
            e.push(event);
        }
    }

    /// Snapshot of retained events.
    pub fn events(&self) -> Vec<LogEvent> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// Number of retained events.
    pub fn len(&self) -> usize {
        self.events.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// True when nothing was logged.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Services handed to backends at instantiation.
#[derive(Debug, Clone, Default)]
pub struct HostServices {
    /// Sink record store.
    pub recorder: Recorder,
    /// Node logs.
    pub logs: Logs,
    /// Sizing for the engine-owned arena.
    pub arena_config: ArenaConfig,
    /// The arena, created on first use.
    pub arena_cell: Arc<OnceLock<Arc<Arena>>>,
    /// Pipeline package directory; builtins resolve relative config paths against it.
    pub package_dir: Option<std::path::PathBuf>,
    /// Machine-local bindings (`value`/`select` resolution, live edits, module bindings).
    pub bindings: Arc<crate::bindings::Bindings>,
    /// Presentation surfaces of nodes that render for a display host (#30). Owned by the engine,
    /// keyed by pipeline node, removed when the engine stops.
    pub display: Arc<mvf_display::DisplayRegistry>,
}

impl HostServices {
    /// Services with this recorder and defaults otherwise.
    pub fn with_recorder(recorder: Recorder) -> Self {
        Self {
            recorder,
            ..Self::default()
        }
    }

    /// The engine-owned arena, created on first use with [`HostServices::arena_config`].
    pub fn arena(&self) -> Result<Arc<Arena>, ArenaError> {
        if let Some(a) = self.arena_cell.get() {
            return Ok(Arc::clone(a));
        }
        let created = Arena::create(&self.arena_config)?;
        // Another thread may have won the race; keep whichever was stored first.
        Ok(Arc::clone(self.arena_cell.get_or_init(|| created)))
    }

    /// The arena if it was created.
    pub fn arena_if_created(&self) -> Option<Arc<Arena>> {
        self.arena_cell.get().cloned()
    }

    /// Logs a supervisor/host lifecycle event for `node`.
    pub fn log(&self, node: &str, generation: u64, level: &str, message: impl Into<String>) {
        self.logs.push(LogEvent {
            node: node.to_string(),
            generation,
            level: level.to_string(),
            message: message.into(),
        });
    }
}
