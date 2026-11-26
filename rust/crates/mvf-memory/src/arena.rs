//! Engine-owned, file-backed shared-memory arena.
//!
//! The arena is one file mapped by the engine and `mmap`-ed by worker processes (`MVF_ARENA_PATH`),
//! split into fixed-size slots. Each slot is `[192-byte descriptor header | payload]` exactly like the
//! .NET `SharedMemoryArena`, so the existing Python/C++ SDKs read and write it unchanged.
//!
//! Ownership is host-side and RAII:
//! - a [`Reservation`] is an exclusive, writable slot (a host-owned output slot handed to exactly one
//!   producer); dropping it returns the slot;
//! - [`Reservation::seal`] validates the header the producer wrote and turns the slot into a
//!   read-only [`Lease`];
//! - cloning a [`Lease`] adds a reference (fan-out); dropping the last one reclaims the slot and bumps
//!   its **generation**, so a stale [`Handle`] can never resolve to a recycled slot.
//!
//! Only the engine allocates; the free list, reference counts and generations live in host memory
//! behind a mutex and never in the shared mapping, so a worker cannot corrupt them.
//!
//! # Safety model (audited `unsafe`)
//! The mapping is accessed through a raw pointer ([`memmap2::MmapRaw`]). Within the host process the
//! slot-ownership rules above guarantee no aliasing `&mut`: a writable view exists only for an
//! unsealed reservation, and read-only views only for sealed leases, which have no write API. Other
//! processes map the same file **read-write** (output slots must be writable), so the host cannot
//! *enforce* that a misbehaving native worker leaves leased slots alone; the protocol forbids it and
//! the host re-validates every header it reads. This is documented as a trust assumption for workers
//! sharing an arena (see `docs/rust-runtime/stages/04.md`).

use std::fs::{File, OpenOptions};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use memmap2::MmapRaw;
use serde::Serialize;

use crate::descriptor::{Descriptor, DescriptorError, HEADER_SIZE};

/// Arena sizing and placement.
#[derive(Debug, Clone)]
pub struct ArenaConfig {
    /// Bytes per slot including the 192-byte header. Rounded up to a multiple of 64.
    pub slot_size: usize,
    /// Number of slots (bounded admission: at most this many payloads alive at once).
    pub slot_count: usize,
    /// Directory for the backing file. Default: `/dev/shm` on Linux when present, else the temp dir.
    pub dir: Option<PathBuf>,
}

impl Default for ArenaConfig {
    fn default() -> Self {
        Self {
            slot_size: 4 * 1024 * 1024,
            slot_count: 16,
            dir: None,
        }
    }
}

/// Arena failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArenaError {
    /// No free slot (bounded admission).
    #[error("arena full: all {0} slots are in use")]
    Full(usize),
    /// Payload does not fit a slot.
    #[error("payload of {len} bytes exceeds the slot capacity of {capacity}")]
    TooLarge {
        /// Payload length.
        len: usize,
        /// Slot payload capacity.
        capacity: usize,
    },
    /// The offset does not address a slot start inside the arena.
    #[error("invalid handle offset {0}")]
    InvalidOffset(u64),
    /// The handle's generation no longer matches the slot (it was reclaimed).
    #[error("stale handle: slot {slot} is at generation {current}, handle has {handle}")]
    StaleGeneration {
        /// Slot index.
        slot: u32,
        /// Current generation.
        current: u32,
        /// Generation in the handle.
        handle: u32,
    },
    /// A header failed validation.
    #[error("invalid payload header: {0}")]
    Descriptor(#[from] DescriptorError),
    /// The descriptor does not describe the given bytes.
    #[error("descriptor implies {expected} bytes but {actual} were given")]
    LengthMismatch {
        /// From the descriptor.
        expected: u64,
        /// Provided.
        actual: usize,
    },
    /// I/O while creating the backing file.
    #[error("arena I/O: {0}")]
    Io(String),
}

/// A host-side reference to a slot: offset plus generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Handle {
    /// Byte offset of the slot start (what workers receive as `shm.offset`).
    pub offset: u64,
    /// Generation of the slot when the handle was issued.
    pub generation: u32,
}

/// Data-plane counters. "Transfer copies" move existing payload bytes from one place to another;
/// "transform writes" are a producer writing its *new* output (not a copy of its input).
#[derive(Debug, Default)]
pub struct ArenaMetrics {
    /// Payloads published into the arena by copying existing bytes (host heap → arena).
    pub copies_in: AtomicU64,
    /// Bytes copied in.
    pub bytes_copied_in: AtomicU64,
    /// Payloads copied out of the arena into host heap memory.
    pub copies_out: AtomicU64,
    /// Bytes copied out.
    pub bytes_copied_out: AtomicU64,
    /// Producer-written outputs sealed in place (transform writes).
    pub transform_writes: AtomicU64,
    /// Bytes written by producers into their own output slots.
    pub transform_bytes: AtomicU64,
    /// Hand-offs of an existing lease to a consumer without copying.
    pub zero_copy_handoffs: AtomicU64,
    /// Slots reclaimed (last reference dropped).
    pub reclaimed: AtomicU64,
    /// Reservations refused because the arena was full.
    pub admission_refusals: AtomicU64,
}

/// A point-in-time copy of [`ArenaMetrics`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    /// See [`ArenaMetrics::copies_in`].
    pub copies_in: u64,
    /// See [`ArenaMetrics::bytes_copied_in`].
    pub bytes_copied_in: u64,
    /// See [`ArenaMetrics::copies_out`].
    pub copies_out: u64,
    /// See [`ArenaMetrics::bytes_copied_out`].
    pub bytes_copied_out: u64,
    /// See [`ArenaMetrics::transform_writes`].
    pub transform_writes: u64,
    /// See [`ArenaMetrics::transform_bytes`].
    pub transform_bytes: u64,
    /// See [`ArenaMetrics::zero_copy_handoffs`].
    pub zero_copy_handoffs: u64,
    /// See [`ArenaMetrics::reclaimed`].
    pub reclaimed: u64,
    /// See [`ArenaMetrics::admission_refusals`].
    pub admission_refusals: u64,
    /// Slots currently in use.
    pub in_use: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SlotState {
    refs: u32,
    generation: u32,
}

#[derive(Debug)]
struct Inner {
    slots: Vec<SlotState>,
    free: Vec<u32>,
}

/// The arena. Shared as `Arc<Arena>`; leases keep it alive.
#[derive(Debug)]
pub struct Arena {
    path: PathBuf,
    map: ManuallyDrop<MmapRaw>,
    file: Option<File>,
    slot_size: usize,
    slot_count: usize,
    inner: Mutex<Inner>,
    metrics: ArenaMetrics,
}

// SAFETY: all access to the mapping goes through slot ownership enforced by `inner` (see module
// docs); the raw pointer itself is just an address valid for the arena's lifetime.
unsafe impl Send for Arena {}
// SAFETY: as above — shared references only hand out disjoint slot views.
unsafe impl Sync for Arena {}

static ARENA_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Arena {
    /// Creates the backing file and maps it.
    pub fn create(config: &ArenaConfig) -> Result<Arc<Self>, ArenaError> {
        let slot_size = config.slot_size.div_ceil(64) * 64;
        if slot_size <= HEADER_SIZE
            || config.slot_count == 0
            || config.slot_count > u32::MAX as usize
        {
            return Err(ArenaError::Io(
                "slot size must exceed the 192-byte header and slot count must be positive".into(),
            ));
        }
        let total = slot_size
            .checked_mul(config.slot_count)
            .ok_or_else(|| ArenaError::Io("arena size overflows".into()))?;
        let dir = config.dir.clone().unwrap_or_else(default_dir);
        let path = dir.join(format!(
            "mvf-arena-{}-{}.bin",
            std::process::id(),
            ARENA_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let io = |e: std::io::Error| ArenaError::Io(format!("{}: {e}", path.display()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        file.set_len(total as u64).map_err(io)?;
        let map = MmapRaw::map_raw(&file).map_err(io)?;
        Ok(Arc::new(Self {
            path,
            map: ManuallyDrop::new(map),
            file: Some(file),
            slot_size,
            slot_count: config.slot_count,
            inner: Mutex::new(Inner {
                slots: vec![SlotState::default(); config.slot_count],
                free: (0..config.slot_count as u32).rev().collect(),
            }),
            metrics: ArenaMetrics::default(),
        }))
    }

    /// Path of the backing file (hand to workers as `MVF_ARENA_PATH`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes per slot including the header.
    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// Payload capacity per slot.
    pub fn capacity(&self) -> usize {
        self.slot_size - HEADER_SIZE
    }

    /// Number of slots.
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Counters.
    pub fn metrics(&self) -> MetricsSnapshot {
        let m = &self.metrics;
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            copies_in: l(&m.copies_in),
            bytes_copied_in: l(&m.bytes_copied_in),
            copies_out: l(&m.copies_out),
            bytes_copied_out: l(&m.bytes_copied_out),
            transform_writes: l(&m.transform_writes),
            transform_bytes: l(&m.transform_bytes),
            zero_copy_handoffs: l(&m.zero_copy_handoffs),
            reclaimed: l(&m.reclaimed),
            admission_refusals: l(&m.admission_refusals),
            in_use: self.in_use() as u64,
        }
    }

    /// Records a zero-copy hand-off of an existing lease to a consumer.
    pub fn note_handoff(&self) {
        self.metrics
            .zero_copy_handoffs
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Records a copy out of the arena (`bytes` long).
    pub fn note_copy_out(&self, bytes: usize) {
        self.metrics.copies_out.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .bytes_copied_out
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Slots currently referenced or reserved.
    pub fn in_use(&self) -> usize {
        self.inner
            .lock()
            .map(|i| self.slot_count - i.free.len())
            .unwrap_or(self.slot_count)
    }

    /// Reserves an exclusive writable slot. Fails with [`ArenaError::Full`] when every slot is in use.
    pub fn reserve(self: &Arc<Self>) -> Result<Reservation, ArenaError> {
        let mut inner = self.lock();
        let Some(slot) = inner.free.pop() else {
            self.metrics
                .admission_refusals
                .fetch_add(1, Ordering::Relaxed);
            return Err(ArenaError::Full(self.slot_count));
        };
        let state = &mut inner.slots[slot as usize];
        state.refs = 1;
        let generation = state.generation;
        Ok(Reservation {
            arena: Arc::clone(self),
            slot,
            generation,
            sealed: false,
        })
    }

    /// Copies `bytes` into a fresh slot and seals it (a counted transfer copy).
    pub fn publish_copy(
        self: &Arc<Self>,
        descriptor: &Descriptor,
        bytes: &[u8],
    ) -> Result<Lease, ArenaError> {
        let mut r = self.reserve()?;
        r.write(descriptor, bytes)?;
        self.metrics.copies_in.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .bytes_copied_in
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        r.seal_unmetered()
    }

    /// Resolves a host-side handle to a live slot, checking alignment, bounds and generation.
    pub fn check(&self, handle: Handle) -> Result<u32, ArenaError> {
        let slot = self.slot_of(handle.offset)?;
        let inner = self.lock();
        let state = inner.slots[slot as usize];
        if state.refs == 0 || state.generation != handle.generation {
            return Err(ArenaError::StaleGeneration {
                slot,
                current: state.generation,
                handle: handle.generation,
            });
        }
        Ok(slot)
    }

    /// The slot index of an offset, if it is a slot start inside the arena.
    pub fn slot_of(&self, offset: u64) -> Result<u32, ArenaError> {
        let size = self.slot_size as u64;
        if !offset.is_multiple_of(size) || offset / size >= self.slot_count as u64 {
            return Err(ArenaError::InvalidOffset(offset));
        }
        Ok((offset / size) as u32)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock only means another thread panicked while holding it; the bookkeeping is
        // plain integers and stays consistent, so keep going.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn slot_ptr(&self, slot: u32) -> *mut u8 {
        // SAFETY: slot < slot_count is guaranteed by every caller (slots come from the free list or
        // `slot_of`), so the offset is within the mapping of slot_size * slot_count bytes.
        unsafe { self.map.as_mut_ptr().add(slot as usize * self.slot_size) }
    }

    fn add_ref(&self, slot: u32) {
        let mut inner = self.lock();
        inner.slots[slot as usize].refs += 1;
    }

    fn release(&self, slot: u32) {
        let mut inner = self.lock();
        let state = &mut inner.slots[slot as usize];
        debug_assert!(state.refs > 0, "release of a free slot");
        state.refs = state.refs.saturating_sub(1);
        if state.refs == 0 {
            state.generation = state.generation.wrapping_add(1);
            inner.free.push(slot);
            self.metrics.reclaimed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // Unmap, close, then delete: Windows refuses to delete a file that is still mapped or open.
        // SAFETY: `map` is never used after this point; leases and reservations hold an `Arc<Arena>`,
        // so none can outlive the arena.
        unsafe { ManuallyDrop::drop(&mut self.map) };
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn default_dir() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if cfg!(target_os = "linux") && shm.is_dir() {
        return shm.to_path_buf();
    }
    std::env::temp_dir()
}

/// An exclusive, writable slot. Dropping it unsealed returns the slot to the arena.
#[derive(Debug)]
pub struct Reservation {
    arena: Arc<Arena>,
    slot: u32,
    generation: u32,
    sealed: bool,
}

impl Reservation {
    /// Offset to hand to a producer (`out.offset`).
    pub fn offset(&self) -> u64 {
        self.slot as u64 * self.arena.slot_size as u64
    }

    /// Payload capacity (`out.capacity`).
    pub fn capacity(&self) -> usize {
        self.arena.capacity()
    }

    /// Host-side handle.
    pub fn handle(&self) -> Handle {
        Handle {
            offset: self.offset(),
            generation: self.generation,
        }
    }

    /// The arena.
    pub fn arena(&self) -> &Arc<Arena> {
        &self.arena
    }

    /// Writes header + payload from the host.
    pub fn write(&mut self, descriptor: &Descriptor, bytes: &[u8]) -> Result<(), ArenaError> {
        let expected = descriptor.byte_len().ok_or(DescriptorError::Overflow)?;
        if expected != bytes.len() as u64 {
            return Err(ArenaError::LengthMismatch {
                expected,
                actual: bytes.len(),
            });
        }
        if bytes.len() > self.capacity() {
            return Err(ArenaError::TooLarge {
                len: bytes.len(),
                capacity: self.capacity(),
            });
        }
        let slot = self.slot_mut();
        descriptor.write_header(&mut slot[..HEADER_SIZE])?;
        slot[HEADER_SIZE..HEADER_SIZE + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Writable payload area (for in-process producers writing their output in place).
    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.slot_mut()[HEADER_SIZE..]
    }

    /// Writes only the header (after an in-place payload write).
    pub fn write_header(&mut self, descriptor: &Descriptor) -> Result<(), ArenaError> {
        let len = descriptor.byte_len().ok_or(DescriptorError::Overflow)?;
        if len > self.capacity() as u64 {
            return Err(ArenaError::TooLarge {
                len: len as usize,
                capacity: self.capacity(),
            });
        }
        descriptor.write_header(&mut self.slot_mut()[..HEADER_SIZE])?;
        Ok(())
    }

    fn slot_mut(&mut self) -> &mut [u8] {
        // SAFETY: an unsealed reservation is the only reference to its slot in this process
        // (refs == 1 held by `self`, no lease exists yet); `&mut self` prevents aliasing here.
        unsafe {
            std::slice::from_raw_parts_mut(self.arena.slot_ptr(self.slot), self.arena.slot_size)
        }
    }

    /// Validates the header the producer wrote and converts the slot into a read-only lease. Counted
    /// as a **transform write** (the producer wrote new output in place).
    pub fn seal(self) -> Result<Lease, ArenaError> {
        let arena = Arc::clone(&self.arena);
        let lease = self.seal_unmetered()?;
        arena
            .metrics
            .transform_writes
            .fetch_add(1, Ordering::Relaxed);
        arena
            .metrics
            .transform_bytes
            .fetch_add(lease.len as u64, Ordering::Relaxed);
        Ok(lease)
    }

    fn seal_unmetered(mut self) -> Result<Lease, ArenaError> {
        let capacity = self.capacity();
        let slot = self.slot_mut();
        let descriptor = Descriptor::read_header(&slot[..HEADER_SIZE])?;
        let len = descriptor.byte_len().ok_or(DescriptorError::Overflow)?;
        if len > capacity as u64 {
            return Err(ArenaError::TooLarge {
                len: len as usize,
                capacity,
            });
        }
        self.sealed = true;
        Ok(Lease {
            arena: Arc::clone(&self.arena),
            slot: self.slot,
            generation: self.generation,
            len: len as usize,
            descriptor,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.sealed {
            self.arena.release(self.slot);
        }
    }
}

/// A read-only, reference-counted lease on a sealed slot.
pub struct Lease {
    arena: Arc<Arena>,
    slot: u32,
    generation: u32,
    len: usize,
    descriptor: Descriptor,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("len", &self.len)
            .finish()
    }
}

impl Lease {
    /// Validated descriptor.
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Slot offset (`shm.offset` for workers).
    pub fn offset(&self) -> u64 {
        self.slot as u64 * self.arena.slot_size as u64
    }

    /// Host-side handle including the generation.
    pub fn handle(&self) -> Handle {
        Handle {
            offset: self.offset(),
            generation: self.generation,
        }
    }

    /// The arena.
    pub fn arena(&self) -> &Arc<Arena> {
        &self.arena
    }

    /// Read-only payload view (zero copy).
    pub fn payload(&self) -> &[u8] {
        // SAFETY: the slot is sealed and referenced by this lease; the host never writes a sealed
        // slot (no API exists), so the view is immutable for the lease's lifetime within this
        // process. See the module docs for the cross-process trust assumption.
        unsafe {
            std::slice::from_raw_parts(self.arena.slot_ptr(self.slot).add(HEADER_SIZE), self.len)
        }
    }
}

impl Clone for Lease {
    fn clone(&self) -> Self {
        self.arena.add_ref(self.slot);
        Self {
            arena: Arc::clone(&self.arena),
            slot: self.slot,
            generation: self.generation,
            len: self.len,
            descriptor: self.descriptor.clone(),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.arena.release(self.slot);
    }
}
