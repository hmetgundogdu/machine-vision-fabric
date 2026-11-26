//! Warm worker pool: parity with the .NET `WarmWorkerPool`. A few pre-warmed spare workers per
//! node, each fully handshaked (`hello`, and `ready` after any warmup) and blocked on stdin. A
//! restart or re-open takes a spare instantly instead of paying the cold start (process spawn plus
//! model load or device connect), and the pool tops itself up in the background. An empty pool
//! falls back to a cold spawn: always correct, just not always warm.
//!
//! A spare holds no state and no config. The node sends `configure` and restores state after taking
//! one, exactly as after a cold spawn. With a private arena (`spec.memory.arena: private`) each spare
//! comes with its own arena, mapped only by that process.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use mvf_engine::NodeError;
use mvf_memory::Arena;

use crate::process::WorkerProcess;

/// What the child announced in its handshake.
#[derive(Debug, Clone)]
pub(crate) struct Hello {
    pub(crate) capability: String,
    pub(crate) features: Vec<String>,
}

/// A ready spare.
pub(crate) struct Spare {
    pub(crate) process: WorkerProcess,
    pub(crate) hello: Hello,
    /// The spare's own arena, in private-arena mode.
    pub(crate) arena: Option<Arc<Arena>>,
}

type SpawnFn = dyn Fn() -> Result<Spare, NodeError> + Send + Sync;

/// Spares for one node (one launch recipe).
pub(crate) struct WarmPool {
    target: usize,
    ready: Mutex<VecDeque<Spare>>,
    warming: AtomicUsize,
    hits: AtomicU64,
    spawn: Arc<SpawnFn>,
}

impl std::fmt::Debug for WarmPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarmPool")
            .field("target", &self.target)
            .field("ready", &self.ready_count())
            .field("hits", &self.hits())
            .finish()
    }
}

impl WarmPool {
    /// A pool that starts warming `target` spares in the background.
    pub(crate) fn start(target: usize, spawn: Arc<SpawnFn>) -> Arc<Self> {
        let pool = Arc::new(Self {
            target,
            ready: Mutex::new(VecDeque::new()),
            warming: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            spawn,
        });
        pool.replenish();
        pool
    }

    /// Spares ready now.
    pub(crate) fn ready_count(&self) -> usize {
        self.ready.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Acquisitions served warm.
    pub(crate) fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// A ready spare, if any (a spare that died while idle is discarded); tops the pool up.
    pub(crate) fn acquire(self: &Arc<Self>) -> Option<Spare> {
        let spare = loop {
            let next = self.ready.lock().ok().and_then(|mut r| r.pop_front());
            match next {
                None => break None,
                Some(mut s) => {
                    if s.process.has_exited() {
                        let _ = s.process.kill(); // died while idle: discard, try the next
                        continue;
                    }
                    break Some(s);
                }
            }
        };
        if spare.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        self.replenish();
        spare
    }

    /// Warms spares until `ready + warming` reaches the target. Warmup threads hold only a weak
    /// reference: a pool dropped mid-warmup kills the late spare instead of leaking it.
    fn replenish(self: &Arc<Self>) {
        loop {
            let have = self.ready_count() + self.warming.load(Ordering::SeqCst);
            if have >= self.target {
                return;
            }
            self.warming.fetch_add(1, Ordering::SeqCst);
            let weak: Weak<Self> = Arc::downgrade(self);
            let spawn = Arc::clone(&self.spawn);
            let started = std::thread::Builder::new()
                .name("mvf-warm-spare".into())
                .spawn(move || {
                    let spare = spawn();
                    if let Some(pool) = weak.upgrade() {
                        if let (Ok(spare), Ok(mut ready)) = (spare, pool.ready.lock()) {
                            ready.push_back(spare);
                        }
                        pool.warming.fetch_sub(1, Ordering::SeqCst);
                    }
                    // A failed warmup just means one fewer spare; the next cold spawn reports why.
                });
            if started.is_err() {
                self.warming.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// Pools by launch recipe, shared by every generation of a node (a restart takes a spare the
/// previous generation's pool warmed). Weak entries: the pool lives exactly as long as a node
/// generation holds it, and dropping it kills the idle spares.
#[derive(Debug, Clone, Default)]
pub(crate) struct PoolRegistry {
    pools: Arc<Mutex<HashMap<String, Weak<WarmPool>>>>,
}

impl PoolRegistry {
    pub(crate) fn get_or_start(
        &self,
        key: &str,
        target: usize,
        spawn: impl FnOnce() -> Arc<SpawnFn>,
    ) -> Option<Arc<WarmPool>> {
        let mut pools = self.pools.lock().ok()?;
        if let Some(pool) = pools.get(key).and_then(Weak::upgrade) {
            return Some(pool);
        }
        let pool = WarmPool::start(target, spawn());
        pools.retain(|_, w| w.strong_count() > 0);
        pools.insert(key.to_string(), Arc::downgrade(&pool));
        Some(pool)
    }
}
