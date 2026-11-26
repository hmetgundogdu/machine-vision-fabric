//! The shared-memory presentation surface, format v1 (`protocol/display-surface.md`).
//!
//! One file per window. Three frame buffers give latest-frame semantics: the node (writer) always
//! has a buffer that is neither the published one nor the one the host is copying, so presenting
//! never blocks, whatever the host does. The host (reader) copies the latest frame out and verifies
//! its CRC-32; a frame the host could not verify is skipped, never shown torn.
//!
//! Every shared field has exactly one writer process, and nothing needs compare-and-swap, so a
//! producer without atomics (the Python SDK) speaks the same format. Ownership is a file lock on a
//! sidecar file: one writer (the node generation that holds `<surface>.writer.lock`) and one
//! host (`<surface>.host.lock`); both are released by the OS when the process dies.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memmap2::MmapMut;

/// File magic.
pub const MAGIC: &[u8; 8] = b"MVFSURF1";
/// Format version.
pub const VERSION: u32 = 1;
/// Header page size.
pub const HEADER_SIZE: usize = 4096;
/// Frame buffers per surface.
pub const BUFFERS: usize = 3;
/// Input ring slots.
pub const RING_SLOTS: usize = 256;
/// Bytes per input ring slot.
pub const SLOT_SIZE: usize = 64;
/// Largest surface edge.
pub const MAX_EDGE: u32 = 8192;
const PAGE: usize = 4096;

// Header (written once by the engine).
const H_VERSION: usize = 8;
const H_BUFFERS: usize = 12;
const H_MAX_W: usize = 16;
const H_MAX_H: usize = 20;
const H_CAPACITY: usize = 24;
const H_BUFFERS_OFF: usize = 32;
const H_RING_OFF: usize = 40;
const H_RING_SLOTS: usize = 48;
const H_SLOT_SIZE: usize = 52;
const H_INDEX: usize = 56;
const H_CRC: usize = 60;
// Writer block (node).
const W_PUBLISHED: usize = 256;
const W_PID: usize = 264;
const W_PRESENTED: usize = 272;
const W_TAIL: usize = 280;
// Reader block (host).
const R_READING: usize = 512;
const R_PID: usize = 520;
const R_HEAD: usize = 528;
// Host status (host, CRC-protected).
const S_BASE: usize = 1024;
const S_LEN: usize = 52;
// Frame metadata, one 64-byte record per buffer (node, CRC over the record and the pixels).
const M_BASE: usize = 2048;
const M_STRIDE: usize = 64;
const M_LEN: usize = 40;

/// Host status flag: a host has this surface open.
pub const FLAG_ATTACHED: u32 = 1;
/// Host status flag: the window has keyboard focus.
pub const FLAG_FOCUSED: u32 = 2;
/// Host status flag: the user closed the window; the host shows this surface no more.
pub const FLAG_CLOSED: u32 = 4;
/// Host status flag: the window is mapped and not minimized.
pub const FLAG_VISIBLE: u32 = 8;

/// Modifier bits on input events.
pub mod modifiers {
    /// Shift.
    pub const SHIFT: u16 = 1;
    /// Control.
    pub const CTRL: u16 = 2;
    /// Alt / Option.
    pub const ALT: u16 = 4;
    /// Super / Command / Windows.
    pub const SUPER: u16 = 8;
}

/// Surface errors.
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// I/O.
    #[error("display surface {path}: {source}")]
    Io {
        /// File.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// The file is not a valid v1 surface (it is untrusted input to the host).
    #[error("display surface {path}: {reason}")]
    Invalid {
        /// File.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// Another host already presents this surface.
    #[error("display surface {0} is already presented by another host")]
    Busy(PathBuf),
    /// A frame that does not fit the surface.
    #[error("frame rejected: {0}")]
    Frame(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SurfaceError + '_ {
    move |source| SurfaceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Pixel formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PixelFormat {
    /// 8-bit blue, green, red, alpha.
    Bgra8,
    /// 8-bit red, green, blue, alpha.
    Rgba8,
    /// 8-bit gray.
    Gray8,
    /// 8-bit red, green, blue.
    Rgb8,
    /// 8-bit blue, green, red.
    Bgr8,
}

impl PixelFormat {
    /// Wire code.
    pub fn code(self) -> u32 {
        match self {
            Self::Bgra8 => 1,
            Self::Rgba8 => 2,
            Self::Gray8 => 3,
            Self::Rgb8 => 4,
            Self::Bgr8 => 5,
        }
    }

    /// From the wire code.
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            1 => Self::Bgra8,
            2 => Self::Rgba8,
            3 => Self::Gray8,
            4 => Self::Rgb8,
            5 => Self::Bgr8,
            _ => return None,
        })
    }

    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Gray8 => 1,
            Self::Rgb8 | Self::Bgr8 => 3,
        }
    }

    /// Pixel `(x, y)` of a frame as `0x00RRGGBB`.
    #[inline]
    pub fn rgb(self, data: &[u8], offset: usize) -> u32 {
        let px = |i: usize| u32::from(data.get(offset + i).copied().unwrap_or(0));
        match self {
            Self::Bgra8 | Self::Bgr8 => (px(2) << 16) | (px(1) << 8) | px(0),
            Self::Rgba8 | Self::Rgb8 => (px(0) << 16) | (px(1) << 8) | px(2),
            Self::Gray8 => {
                let g = px(0);
                (g << 16) | (g << 8) | g
            }
        }
    }
}

/// A frame to present.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per row (`>= width × bytes per pixel`).
    pub stride: u32,
    /// Pixel format.
    pub format: PixelFormat,
    /// `stride × height` bytes.
    pub data: &'a [u8],
}

/// A frame the host copied out and verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// Frame sequence (per surface, from 1).
    pub seq: u64,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    /// Format.
    pub format: PixelFormat,
    /// When the node presented it (Unix microseconds).
    pub timestamp_us: u64,
}

/// Outcome of a present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// Published as frame `seq`.
    Shown {
        /// Sequence.
        seq: u64,
    },
    /// Another generation of the node owns the surface (it is being replaced); nothing written.
    Standby,
}

/// What the host reports to the node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostStatus {
    /// Increments on every status write.
    pub counter: u64,
    /// Last heartbeat (Unix milliseconds).
    pub heartbeat_ms: u64,
    /// `FLAG_*` bits.
    pub flags: u32,
    /// Window content size the host would like frames in (physical pixels), 0 = no preference.
    pub requested_width: u32,
    /// Requested height.
    pub requested_height: u32,
    /// Window scale factor × 1000 (DPI).
    pub scale_milli: u32,
    /// Frames the host has shown.
    pub shown: u64,
    /// Input events dropped because the node did not consume them.
    pub dropped_events: u64,
    /// Host process id.
    pub host_pid: u32,
}

impl HostStatus {
    /// Whether a host is attached and alive (heartbeat within `stale`).
    pub fn attached(&self, stale: Duration) -> bool {
        self.flags & FLAG_ATTACHED != 0
            && now_ms().saturating_sub(self.heartbeat_ms) <= stale.as_millis() as u64
    }

    /// Whether the user closed the window.
    pub fn closed(&self) -> bool {
        self.flags & FLAG_CLOSED != 0
    }

    /// Whether the window has keyboard focus.
    pub fn focused(&self) -> bool {
        self.flags & FLAG_FOCUSED != 0
    }

    fn encode(&self) -> [u8; S_LEN + 4] {
        let mut b = [0u8; S_LEN + 4];
        b[0..8].copy_from_slice(&self.counter.to_le_bytes());
        b[8..16].copy_from_slice(&self.heartbeat_ms.to_le_bytes());
        b[16..20].copy_from_slice(&self.flags.to_le_bytes());
        b[20..24].copy_from_slice(&self.requested_width.to_le_bytes());
        b[24..28].copy_from_slice(&self.requested_height.to_le_bytes());
        b[28..32].copy_from_slice(&self.scale_milli.to_le_bytes());
        b[32..40].copy_from_slice(&self.shown.to_le_bytes());
        b[40..48].copy_from_slice(&self.dropped_events.to_le_bytes());
        b[48..52].copy_from_slice(&self.host_pid.to_le_bytes());
        let crc = crc32fast::hash(&b[..S_LEN]);
        b[S_LEN..S_LEN + 4].copy_from_slice(&crc.to_le_bytes());
        b
    }

    fn decode(b: &[u8]) -> Option<Self> {
        let crc = u32_at(b, S_LEN);
        if crc32fast::hash(&b[..S_LEN]) != crc {
            return None;
        }
        let s = Self {
            counter: u64_at(b, 0),
            heartbeat_ms: u64_at(b, 8),
            flags: u32_at(b, 16),
            requested_width: u32_at(b, 20),
            requested_height: u32_at(b, 24),
            scale_milli: u32_at(b, 28),
            shown: u64_at(b, 32),
            dropped_events: u64_at(b, 40),
            host_pid: u32_at(b, 48),
        };
        (s.counter > 0).then_some(s)
    }
}

/// Input and window events, host → node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputKind {
    /// Pointer moved (frame coordinates).
    PointerMove,
    /// Button pressed (`code` = button: 0 left, 1 right, 2 middle).
    PointerDown,
    /// Button released.
    PointerUp,
    /// Wheel (`dx`, `dy` in lines).
    Wheel,
    /// Key pressed (`text` = logical key, e.g. `a`, `Enter`, `ArrowLeft`; `code` 1 = repeat).
    KeyDown,
    /// Key released.
    KeyUp,
    /// Focus changed (`code` 1 = gained).
    Focus,
    /// Window content resized (`x`, `y` = physical width, height).
    Resize,
    /// The user asked to close the window.
    CloseRequested,
    /// The pointer left the window.
    PointerLeave,
}

impl InputKind {
    fn code(self) -> u16 {
        match self {
            Self::PointerMove => 1,
            Self::PointerDown => 2,
            Self::PointerUp => 3,
            Self::Wheel => 4,
            Self::KeyDown => 5,
            Self::KeyUp => 6,
            Self::Focus => 7,
            Self::Resize => 8,
            Self::CloseRequested => 9,
            Self::PointerLeave => 10,
        }
    }

    fn from_code(c: u16) -> Option<Self> {
        Some(match c {
            1 => Self::PointerMove,
            2 => Self::PointerDown,
            3 => Self::PointerUp,
            4 => Self::Wheel,
            5 => Self::KeyDown,
            6 => Self::KeyUp,
            7 => Self::Focus,
            8 => Self::Resize,
            9 => Self::CloseRequested,
            10 => Self::PointerLeave,
            _ => return None,
        })
    }

    /// Keyboard input (needs the `keyboard` grant).
    pub fn is_keyboard(self) -> bool {
        matches!(self, Self::KeyDown | Self::KeyUp)
    }

    /// Pointer input (needs the `mouse` grant).
    pub fn is_pointer(self) -> bool {
        matches!(
            self,
            Self::PointerMove
                | Self::PointerDown
                | Self::PointerUp
                | Self::Wheel
                | Self::PointerLeave
        )
    }

    /// Window lifecycle (focus, resize, close): always delivered, triggers no machine action.
    pub fn is_window(self) -> bool {
        matches!(self, Self::Focus | Self::Resize | Self::CloseRequested)
    }

    /// Stable name (SDKs, logs).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PointerMove => "pointer-move",
            Self::PointerDown => "pointer-down",
            Self::PointerUp => "pointer-up",
            Self::Wheel => "wheel",
            Self::KeyDown => "key-down",
            Self::KeyUp => "key-up",
            Self::Focus => "focus",
            Self::Resize => "resize",
            Self::CloseRequested => "close-requested",
            Self::PointerLeave => "pointer-leave",
        }
    }
}

/// One event in the input ring.
#[derive(Debug, Clone, PartialEq)]
pub struct InputEvent {
    /// Kind.
    pub kind: InputKind,
    /// `modifiers::*` bits.
    pub modifiers: u16,
    /// Button, repeat or focus flag (see [`InputKind`]).
    pub code: u32,
    /// Pointer x in frame pixels (or resize width).
    pub x: f32,
    /// Pointer y in frame pixels (or resize height).
    pub y: f32,
    /// Wheel x.
    pub dx: f32,
    /// Wheel y.
    pub dy: f32,
    /// Logical key, at most 24 UTF-8 bytes.
    pub text: String,
}

impl InputEvent {
    /// An event with no coordinates.
    pub fn new(kind: InputKind) -> Self {
        Self {
            kind,
            modifiers: 0,
            code: 0,
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            text: String::new(),
        }
    }

    /// A pointer event at frame coordinates.
    pub fn pointer(kind: InputKind, x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            ..Self::new(kind)
        }
    }

    /// A key event.
    pub fn key(kind: InputKind, key: &str) -> Self {
        Self {
            text: key.to_string(),
            ..Self::new(kind)
        }
    }

    fn encode(&self, seq: u64) -> [u8; SLOT_SIZE] {
        let mut b = [0u8; SLOT_SIZE];
        b[0..8].copy_from_slice(&seq.to_le_bytes());
        b[8..10].copy_from_slice(&self.kind.code().to_le_bytes());
        b[10..12].copy_from_slice(&self.modifiers.to_le_bytes());
        b[12..16].copy_from_slice(&self.code.to_le_bytes());
        b[16..20].copy_from_slice(&self.x.to_le_bytes());
        b[20..24].copy_from_slice(&self.y.to_le_bytes());
        b[24..28].copy_from_slice(&self.dx.to_le_bytes());
        b[28..32].copy_from_slice(&self.dy.to_le_bytes());
        let mut end = self.text.len().min(24);
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        b[32..32 + end].copy_from_slice(&self.text.as_bytes()[..end]);
        let crc = crc32fast::hash(&b[..56]);
        b[56..60].copy_from_slice(&crc.to_le_bytes());
        b
    }

    fn decode(b: &[u8]) -> Option<(u64, Self)> {
        if crc32fast::hash(&b[..56]) != u32_at(b, 56) {
            return None;
        }
        let kind = InputKind::from_code(u16::from_le_bytes([b[8], b[9]]))?;
        let text = &b[32..56];
        let text_len = text.iter().position(|&c| c == 0).unwrap_or(24);
        Some((
            u64_at(b, 0),
            Self {
                kind,
                modifiers: u16::from_le_bytes([b[10], b[11]]),
                code: u32_at(b, 12),
                x: f32::from_le_bytes([b[16], b[17], b[18], b[19]]),
                y: f32::from_le_bytes([b[20], b[21], b[22], b[23]]),
                dx: f32::from_le_bytes([b[24], b[25], b[26], b[27]]),
                dy: f32::from_le_bytes([b[28], b[29], b[30], b[31]]),
                text: String::from_utf8_lossy(&text[..text_len]).into_owned(),
            },
        ))
    }
}

/// Surface layout, validated from the (untrusted) header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Largest frame width.
    pub max_width: u32,
    /// Largest frame height.
    pub max_height: u32,
    /// Bytes per buffer.
    pub capacity: usize,
    /// Offset of buffer 0.
    pub buffers_offset: usize,
    /// Offset of the input ring.
    pub ring_offset: usize,
    /// Ring slots.
    pub ring_slots: usize,
    /// Window index within the node.
    pub index: u32,
}

impl Geometry {
    fn for_size(max_width: u32, max_height: u32, index: u32) -> Self {
        let capacity = align(max_width as usize * max_height as usize * 4);
        Self {
            max_width,
            max_height,
            capacity,
            buffers_offset: align(HEADER_SIZE + RING_SLOTS * SLOT_SIZE),
            ring_offset: HEADER_SIZE,
            ring_slots: RING_SLOTS,
            index,
        }
    }

    /// Total file size.
    pub fn file_len(&self) -> usize {
        self.buffers_offset + BUFFERS * self.capacity
    }

    fn parse(path: &Path, map: &[u8]) -> Result<Self, SurfaceError> {
        let bad = |reason: &str| SurfaceError::Invalid {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        if map.len() < HEADER_SIZE {
            return Err(bad("shorter than the header"));
        }
        if &map[..8] != MAGIC || u32_at(map, H_VERSION) != VERSION {
            return Err(bad("not an MVF surface v1 (magic/version)"));
        }
        if crc32fast::hash(&map[..H_CRC]) != u32_at(map, H_CRC) {
            return Err(bad("header checksum mismatch"));
        }
        let g = Self {
            max_width: u32_at(map, H_MAX_W),
            max_height: u32_at(map, H_MAX_H),
            capacity: u64_at(map, H_CAPACITY) as usize,
            buffers_offset: u64_at(map, H_BUFFERS_OFF) as usize,
            ring_offset: u64_at(map, H_RING_OFF) as usize,
            ring_slots: u32_at(map, H_RING_SLOTS) as usize,
            index: u32_at(map, H_INDEX),
        };
        let expected = Self::for_size(g.max_width, g.max_height, g.index);
        if u32_at(map, H_BUFFERS) as usize != BUFFERS
            || u32_at(map, H_SLOT_SIZE) as usize != SLOT_SIZE
            || !(1..=MAX_EDGE).contains(&g.max_width)
            || !(1..=MAX_EDGE).contains(&g.max_height)
            || g != expected
        {
            return Err(bad("inconsistent geometry"));
        }
        if map.len() < g.file_len() {
            return Err(bad("file is shorter than its geometry"));
        }
        Ok(g)
    }
}

fn align(n: usize) -> usize {
    n.div_ceil(PAGE) * PAGE
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

/// Unix milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// The sidecar lock file the writing generation holds.
pub fn writer_lock_path(surface: &Path) -> PathBuf {
    sidecar(surface, "writer.lock")
}

/// The sidecar lock file the presenting host holds.
pub fn host_lock_path(surface: &Path) -> PathBuf {
    sidecar(surface, "host.lock")
}

fn sidecar(surface: &Path, suffix: &str) -> PathBuf {
    let mut s = surface.as_os_str().to_owned();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

fn open_rw(path: &Path) -> Result<File, SurfaceError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io(path))
}

#[cfg(unix)]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600)
}

#[cfg(not(unix))]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    options
}

/// Creates a surface file (and its lock sidecars) sized for `max_width × max_height` BGRA.
pub fn create(
    path: &Path,
    max_width: u32,
    max_height: u32,
    index: u32,
) -> Result<Geometry, SurfaceError> {
    if !(1..=MAX_EDGE).contains(&max_width) || !(1..=MAX_EDGE).contains(&max_height) {
        return Err(SurfaceError::Frame(format!(
            "surface {max_width}x{max_height} is outside 1..={MAX_EDGE}"
        )));
    }
    let g = Geometry::for_size(max_width, max_height, index);
    let file = private(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true),
    )
    .open(path)
    .map_err(io(path))?;
    file.set_len(g.file_len() as u64).map_err(io(path))?;
    // SAFETY: the file was just created at its final size and is mapped shared.
    let mut map = unsafe { MmapMut::map_mut(&file) }.map_err(io(path))?;
    map[..8].copy_from_slice(MAGIC);
    map[H_VERSION..H_VERSION + 4].copy_from_slice(&VERSION.to_le_bytes());
    map[H_BUFFERS..H_BUFFERS + 4].copy_from_slice(&(BUFFERS as u32).to_le_bytes());
    map[H_MAX_W..H_MAX_W + 4].copy_from_slice(&max_width.to_le_bytes());
    map[H_MAX_H..H_MAX_H + 4].copy_from_slice(&max_height.to_le_bytes());
    map[H_CAPACITY..H_CAPACITY + 8].copy_from_slice(&(g.capacity as u64).to_le_bytes());
    map[H_BUFFERS_OFF..H_BUFFERS_OFF + 8].copy_from_slice(&(g.buffers_offset as u64).to_le_bytes());
    map[H_RING_OFF..H_RING_OFF + 8].copy_from_slice(&(g.ring_offset as u64).to_le_bytes());
    map[H_RING_SLOTS..H_RING_SLOTS + 4].copy_from_slice(&(g.ring_slots as u32).to_le_bytes());
    map[H_SLOT_SIZE..H_SLOT_SIZE + 4].copy_from_slice(&(SLOT_SIZE as u32).to_le_bytes());
    map[H_INDEX..H_INDEX + 4].copy_from_slice(&index.to_le_bytes());
    let crc = crc32fast::hash(&map[..H_CRC]);
    map[H_CRC..H_CRC + 4].copy_from_slice(&crc.to_le_bytes());
    map.flush().map_err(io(path))?;
    for lock in [writer_lock_path(path), host_lock_path(path)] {
        private(OpenOptions::new().write(true).create(true).truncate(false))
            .open(&lock)
            .map_err(io(&lock))?;
    }
    Ok(g)
}

fn map_surface(path: &Path) -> Result<(MmapMut, Geometry), SurfaceError> {
    let file = open_rw(path)?;
    // SAFETY: shared mapping of a surface file; every field is validated before use and the other
    // party is treated as untrusted (bounds, CRC).
    let map = unsafe { MmapMut::map_mut(&file) }.map_err(io(path))?;
    let g = Geometry::parse(path, &map)?;
    Ok((map, g))
}

fn atomic(map: &MmapMut, off: usize) -> &AtomicU64 {
    debug_assert!(off.is_multiple_of(8) && off + 8 <= HEADER_SIZE);
    // SAFETY: `off` is 8-aligned inside the page-aligned mapping's header, which lives as long as
    // `map`; the cell is only ever accessed atomically by Rust code.
    unsafe { &*map.as_ptr().add(off).cast::<AtomicU64>() }
}

/// Copies `out.len()` bytes at `off` without forming a reference to memory another process is
/// writing.
fn copy_out(map: &MmapMut, off: usize, out: &mut [u8]) {
    assert!(off + out.len() <= map.len());
    // SAFETY: bounds checked above; the destination is a private buffer.
    unsafe { std::ptr::copy_nonoverlapping(map.as_ptr().add(off), out.as_mut_ptr(), out.len()) }
}

fn try_lock(file: &File) -> bool {
    file.try_lock().is_ok()
}

/// The node side of a surface.
#[derive(Debug)]
pub struct SurfaceWriter {
    path: PathBuf,
    map: MmapMut,
    geometry: Geometry,
    lock: File,
    owned: bool,
    last_attempt: Option<Instant>,
    seq: u64,
    tail: u64,
}

impl SurfaceWriter {
    /// Opens a surface the engine created. Ownership is taken on first present.
    pub fn open(path: &Path) -> Result<Self, SurfaceError> {
        let (map, geometry) = map_surface(path)?;
        let lock = open_rw(&writer_lock_path(path))?;
        Ok(Self {
            path: path.to_path_buf(),
            map,
            geometry,
            lock,
            owned: false,
            last_attempt: None,
            seq: 0,
            tail: 0,
        })
    }

    /// The surface's geometry.
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The surface file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this writer owns the surface (another generation may, during a replacement).
    pub fn owned(&mut self) -> bool {
        if self.owned {
            return true;
        }
        // A standby generation retries at most every 50 ms: one syscall, never a wait.
        if self
            .last_attempt
            .is_some_and(|t| t.elapsed() < Duration::from_millis(50))
        {
            return false;
        }
        self.last_attempt = Some(Instant::now());
        if !try_lock(&self.lock) {
            return false;
        }
        self.owned = true;
        self.seq = atomic(&self.map, W_PUBLISHED).load(Ordering::Acquire) >> 2;
        self.tail = atomic(&self.map, W_TAIL).load(Ordering::Acquire);
        atomic(&self.map, W_PID).store(u64::from(std::process::id()), Ordering::Release);
        true
    }

    /// Presents a frame (copied into a free buffer). Never blocks.
    pub fn present(&mut self, frame: &Frame<'_>) -> Result<Presented, SurfaceError> {
        let rows = frame.height as usize;
        let row = frame.width as usize * frame.format.bytes_per_pixel() as usize;
        let src_stride = frame.stride as usize;
        if src_stride < row || frame.data.len() < src_stride * rows.saturating_sub(1) + row {
            return Err(SurfaceError::Frame(format!(
                "{} bytes do not hold {}x{} {:?} at stride {}",
                frame.data.len(),
                frame.width,
                frame.height,
                frame.format,
                frame.stride
            )));
        }
        let data = frame.data;
        self.present_with(frame.width, frame.height, frame.format, |out, stride| {
            for y in 0..rows {
                out[y * stride..y * stride + row]
                    .copy_from_slice(&data[y * src_stride..y * src_stride + row]);
            }
        })
    }

    /// Presents a frame rendered directly into the surface buffer (no extra copy): `render`
    /// receives the buffer and its stride.
    pub fn present_with(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
        render: impl FnOnce(&mut [u8], usize),
    ) -> Result<Presented, SurfaceError> {
        let g = self.geometry;
        if width == 0 || height == 0 || width > g.max_width || height > g.max_height {
            return Err(SurfaceError::Frame(format!(
                "{width}x{height} exceeds the surface maximum {}x{}",
                g.max_width, g.max_height
            )));
        }
        if !self.owned() {
            return Ok(Presented::Standby);
        }
        let stride = width as usize * format.bytes_per_pixel() as usize;
        let len = stride * height as usize;
        let published = atomic(&self.map, W_PUBLISHED).load(Ordering::Acquire);
        let reading = atomic(&self.map, R_READING).load(Ordering::Acquire);
        let busy = [
            (published != 0).then_some((published & 3) as usize),
            reading.checked_sub(1).map(|r| r as usize),
        ];
        let idx = (0..BUFFERS)
            .find(|i| !busy.contains(&Some(*i)))
            .unwrap_or(0);
        let off = g.buffers_offset + idx * g.capacity;
        render(&mut self.map[off..off + len], stride);
        self.seq += 1;
        let mut meta = [0u8; M_LEN + 4];
        meta[0..8].copy_from_slice(&self.seq.to_le_bytes());
        meta[8..12].copy_from_slice(&width.to_le_bytes());
        meta[12..16].copy_from_slice(&height.to_le_bytes());
        meta[16..20].copy_from_slice(&(stride as u32).to_le_bytes());
        meta[20..24].copy_from_slice(&format.code().to_le_bytes());
        meta[24..32].copy_from_slice(&now_us().to_le_bytes());
        meta[32..40].copy_from_slice(&(len as u64).to_le_bytes());
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&meta[..M_LEN]);
        hasher.update(&self.map[off..off + len]);
        meta[M_LEN..M_LEN + 4].copy_from_slice(&hasher.finalize().to_le_bytes());
        let m = M_BASE + idx * M_STRIDE;
        self.map[m..m + M_LEN + 4].copy_from_slice(&meta);
        fence(Ordering::Release);
        atomic(&self.map, W_PUBLISHED).store((self.seq << 2) | idx as u64, Ordering::Release);
        atomic(&self.map, W_PRESENTED).fetch_add(1, Ordering::Relaxed);
        Ok(Presented::Shown { seq: self.seq })
    }

    /// The host's last status, if a host ever attached and the record verifies.
    pub fn host(&self) -> Option<HostStatus> {
        let mut b = [0u8; S_LEN + 4];
        copy_out(&self.map, S_BASE, &mut b);
        fence(Ordering::Acquire);
        HostStatus::decode(&b)
    }

    /// Drains routed input into `out`. Returns how many events were lost (overwritten before the
    /// node read them). Only the owning generation consumes input.
    pub fn poll_input(&mut self, out: &mut Vec<InputEvent>) -> u64 {
        if !self.owned() {
            return 0;
        }
        let g = self.geometry;
        let mut lost = 0;
        for _ in 0..g.ring_slots {
            let off = g.ring_offset + (self.tail as usize % g.ring_slots) * SLOT_SIZE;
            let mut b = [0u8; SLOT_SIZE];
            copy_out(&self.map, off, &mut b);
            fence(Ordering::Acquire);
            let Some((seq, event)) = InputEvent::decode(&b) else {
                break;
            };
            if seq <= self.tail {
                break;
            }
            if seq > self.tail + 1 {
                lost += seq - (self.tail + 1);
            }
            self.tail = seq;
            out.push(event);
        }
        atomic(&self.map, W_TAIL).store(self.tail, Ordering::Release);
        lost
    }
}

impl Drop for SurfaceWriter {
    fn drop(&mut self) {
        if self.owned {
            atomic(&self.map, W_PID).store(0, Ordering::Release);
        }
    }
}

/// The host side of a surface.
#[derive(Debug)]
pub struct SurfaceReader {
    path: PathBuf,
    map: MmapMut,
    geometry: Geometry,
    _lock: File,
    head: u64,
    last_seq: u64,
    status: HostStatus,
    /// Reads rejected by the CRC (torn or not yet visible); retried on the next poll.
    pub rejected: u64,
}

impl SurfaceReader {
    /// Opens a surface for presentation; [`SurfaceError::Busy`] when another host has it.
    pub fn open(path: &Path) -> Result<Self, SurfaceError> {
        let (map, geometry) = map_surface(path)?;
        let lock = open_rw(&host_lock_path(path))?;
        if !try_lock(&lock) {
            return Err(SurfaceError::Busy(path.to_path_buf()));
        }
        let stored_head = atomic(&map, R_HEAD).load(Ordering::Acquire);
        let tail = atomic(&map, W_TAIL).load(Ordering::Acquire);
        let mut b = [0u8; S_LEN + 4];
        copy_out(&map, S_BASE, &mut b);
        let previous = HostStatus::decode(&b).unwrap_or_default();
        atomic(&map, R_PID).store(u64::from(std::process::id()), Ordering::Release);
        atomic(&map, R_READING).store(0, Ordering::Release);
        let mut reader = Self {
            path: path.to_path_buf(),
            map,
            geometry,
            _lock: lock,
            head: stored_head.max(tail),
            last_seq: 0,
            status: HostStatus {
                counter: previous.counter,
                shown: previous.shown,
                dropped_events: previous.dropped_events,
                host_pid: std::process::id(),
                ..HostStatus::default()
            },
            rejected: 0,
        };
        reader.update_status(|s| s.flags = FLAG_ATTACHED | FLAG_VISIBLE);
        Ok(reader)
    }

    /// The surface file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The surface's geometry.
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The sequence of the newest published frame (0 = none yet).
    pub fn published_seq(&self) -> u64 {
        atomic(&self.map, W_PUBLISHED).load(Ordering::Acquire) >> 2
    }

    /// The writing process, if a generation owns the surface.
    pub fn writer_pid(&self) -> Option<u32> {
        let pid = atomic(&self.map, W_PID).load(Ordering::Acquire) as u32;
        (pid != 0).then_some(pid)
    }

    /// Frames the node presented in total.
    pub fn presented(&self) -> u64 {
        atomic(&self.map, W_PRESENTED).load(Ordering::Acquire)
    }

    /// Copies the newest frame newer than the last one taken into `out` and verifies it.
    pub fn take_latest(&mut self, out: &mut Vec<u8>) -> Option<FrameInfo> {
        let g = self.geometry;
        for _ in 0..3 {
            let published = atomic(&self.map, W_PUBLISHED).load(Ordering::Acquire);
            let seq = published >> 2;
            let idx = (published & 3) as usize;
            if published == 0 || seq <= self.last_seq {
                return None;
            }
            if idx >= BUFFERS {
                self.rejected += 1;
                return None;
            }
            atomic(&self.map, R_READING).store(idx as u64 + 1, Ordering::Release);
            fence(Ordering::SeqCst);
            if atomic(&self.map, W_PUBLISHED).load(Ordering::Acquire) != published {
                continue; // a newer frame arrived meanwhile: take that one
            }
            let mut meta = [0u8; M_LEN + 4];
            copy_out(&self.map, M_BASE + idx * M_STRIDE, &mut meta);
            let width = u32_at(&meta, 8);
            let height = u32_at(&meta, 12);
            let stride = u32_at(&meta, 16);
            let format = PixelFormat::from_code(u32_at(&meta, 20));
            let len = u64_at(&meta, 32) as usize;
            let valid = format.is_some_and(|f| {
                (1..=g.max_width).contains(&width)
                    && (1..=g.max_height).contains(&height)
                    && stride as u64 >= u64::from(width) * u64::from(f.bytes_per_pixel())
                    && len as u64 == u64::from(stride) * u64::from(height)
                    && len <= g.capacity
            });
            if !valid {
                atomic(&self.map, R_READING).store(0, Ordering::Release);
                self.rejected += 1;
                continue;
            }
            out.resize(len, 0);
            copy_out(&self.map, g.buffers_offset + idx * g.capacity, out);
            fence(Ordering::Acquire);
            atomic(&self.map, R_READING).store(0, Ordering::Release);
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(&meta[..M_LEN]);
            hasher.update(out);
            if hasher.finalize() != u32_at(&meta, M_LEN) || u64_at(&meta, 0) != seq {
                self.rejected += 1;
                continue;
            }
            self.last_seq = seq;
            self.status.shown += 1;
            return Some(FrameInfo {
                seq,
                width,
                height,
                stride,
                format: format.unwrap_or(PixelFormat::Bgra8),
                timestamp_us: u64_at(&meta, 24),
            });
        }
        None
    }

    /// Updates and publishes the host status (heartbeat included).
    pub fn update_status(&mut self, change: impl FnOnce(&mut HostStatus)) {
        change(&mut self.status);
        self.status.counter += 1;
        self.status.heartbeat_ms = now_ms();
        let b = self.status.encode();
        self.map[S_BASE..S_BASE + b.len()].copy_from_slice(&b);
        fence(Ordering::Release);
    }

    /// The status this host last published.
    pub fn status(&self) -> HostStatus {
        self.status
    }

    /// Queues an event for the node. Returns `false` (and counts a drop) when the node has not
    /// consumed the ring: input never waits for a slow node.
    pub fn push(&mut self, event: &InputEvent) -> bool {
        let g = self.geometry;
        let tail = atomic(&self.map, W_TAIL)
            .load(Ordering::Acquire)
            .min(self.head);
        if self.head - tail >= g.ring_slots as u64 {
            self.status.dropped_events += 1;
            return false;
        }
        let seq = self.head + 1;
        let off = g.ring_offset + (self.head as usize % g.ring_slots) * SLOT_SIZE;
        let b = event.encode(seq);
        self.map[off..off + SLOT_SIZE].copy_from_slice(&b);
        fence(Ordering::Release);
        self.head = seq;
        atomic(&self.map, R_HEAD).store(seq, Ordering::Release);
        true
    }
}

impl Drop for SurfaceReader {
    fn drop(&mut self) {
        // Detach: the node sees the host gone at once instead of waiting for a stale heartbeat.
        self.update_status(|s| s.flags &= !(FLAG_ATTACHED | FLAG_FOCUSED | FLAG_VISIBLE));
        atomic(&self.map, R_PID).store(0, Ordering::Release);
    }
}
