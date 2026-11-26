"""MVF Display Fabric surfaces for Python nodes (#30). See ``protocol/display-surface.md``.

A node whose contract says ``permissions.display.mode: managed-surface`` gets one shared-memory
surface per window (``MVF_DISPLAY_SURFACES``). A separate display host in the user's session
presents them; MVF never draws the node's UI. ``Surface.present()`` never blocks: with no host
attached the frame is simply not shown, and the graph does not notice.

Integrity does not depend on memory ordering, which Python cannot control: every frame carries a
CRC-32 the host verifies before showing it, and every input event a CRC-32 this module verifies
before handing it over. Standard library only.
"""
import mmap
import os
import struct
import time
import zlib

__all__ = ["Surface", "surfaces", "HostStatus", "InputEvent", "FORMATS"]

_MAGIC = b"MVFSURF1"
_HEADER = 4096
_W_PUBLISHED, _W_PID, _W_PRESENTED, _W_TAIL = 256, 264, 272, 280
_R_READING = 512
_S_BASE, _S_LEN = 1024, 52
_M_BASE, _M_STRIDE = 2048, 64
_SLOT = 64

# name -> (wire code, bytes per pixel)
FORMATS = {"bgra8": (1, 4), "rgba8": (2, 4), "gray8": (3, 1), "rgb8": (4, 3), "bgr8": (5, 3)}

_KINDS = {1: "pointer-move", 2: "pointer-down", 3: "pointer-up", 4: "wheel", 5: "key-down",
          6: "key-up", 7: "focus", 8: "resize", 9: "close-requested", 10: "pointer-leave"}

FLAG_ATTACHED, FLAG_FOCUSED, FLAG_CLOSED, FLAG_VISIBLE = 1, 2, 4, 8


class HostStatus:
    """What the display host last reported for a surface."""

    def __init__(self, counter, heartbeat_ms, flags, requested_width, requested_height,
                 scale_milli, shown, dropped_events, host_pid):
        self.counter = counter
        self.heartbeat_ms = heartbeat_ms
        self.flags = flags
        self.requested_width = requested_width
        self.requested_height = requested_height
        self.scale = scale_milli / 1000.0
        self.shown = shown
        self.dropped_events = dropped_events
        self.host_pid = host_pid

    def attached(self, stale_ms=2000):
        """A host has the surface open and its heartbeat is fresh."""
        return bool(self.flags & FLAG_ATTACHED) and \
            time.time() * 1000 - self.heartbeat_ms <= stale_ms

    @property
    def closed(self):
        """The user closed the window."""
        return bool(self.flags & FLAG_CLOSED)

    @property
    def focused(self):
        return bool(self.flags & FLAG_FOCUSED)


class InputEvent:
    """One routed input or window event (coordinates are in frame pixels)."""

    __slots__ = ("kind", "modifiers", "code", "x", "y", "dx", "dy", "text")

    def __init__(self, kind, modifiers, code, x, y, dx, dy, text):
        self.kind = kind
        self.modifiers = modifiers
        self.code = code
        self.x = x
        self.y = y
        self.dx = dx
        self.dy = dy
        self.text = text

    def __repr__(self):
        return "InputEvent({}, x={:.1f}, y={:.1f}, text={!r})".format(
            self.kind, self.x, self.y, self.text)


def _try_lock(f):
    """Non-blocking exclusive lock of the surface's writer lock file."""
    try:
        if os.name == "nt":
            import msvcrt
            f.seek(0)
            msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except OSError:
        return False


def _unlock(f):
    if os.name == "nt":
        import msvcrt
        f.seek(0)
        msvcrt.locking(f.fileno(), msvcrt.LK_UNLCK, 1)
    else:
        import fcntl
        fcntl.flock(f.fileno(), fcntl.LOCK_UN)


class Surface:
    """The node side of one surface (one window).

    The writer lock is held only while one frame is presented or the input is drained: any
    generation of the node may present next and continues the surface's sequence, so a live
    replacement moves the window to the new generation at its first frame.
    """

    def __init__(self, path):
        self.path = path
        self._file = open(path, "r+b")
        self._map = mmap.mmap(self._file.fileno(), 0)
        self._view = memoryview(self._map)
        self._u64 = self._view[:_HEADER].cast("Q")
        header = bytes(self._view[:64])
        if header[:8] != _MAGIC or zlib.crc32(header[:60]) & 0xFFFFFFFF != \
                struct.unpack_from("<I", header, 60)[0]:
            raise ValueError("{} is not an MVF display surface v1".format(path))
        (_version, _buffers, self.max_width, self.max_height, self._capacity, self._buffers_off,
         self._ring_off, self._ring_slots, _slot, self.index) = struct.unpack_from(
            "<IIIIQQQIII", header, 8)
        self._lock_file = open(path + ".writer.lock", "r+b")
        self.lost_events = 0

    def present(self, data, width, height, format="bgra8", stride=None):
        """Publish one frame. Returns True when published, False when another generation is
        presenting at this instant. Never blocks."""
        if format not in FORMATS:
            raise ValueError("unknown pixel format {!r}".format(format))
        code, bpp = FORMATS[format]
        if not (0 < width <= self.max_width and 0 < height <= self.max_height):
            raise ValueError("{}x{} exceeds the surface maximum {}x{}".format(
                width, height, self.max_width, self.max_height))
        row = width * bpp
        stride = stride or row
        src = memoryview(data).cast("B")
        if stride < row or len(src) < stride * (height - 1) + row:
            raise ValueError("{} bytes do not hold {}x{} {}".format(len(src), width, height, format))
        if not _try_lock(self._lock_file):
            return False
        try:
            self._publish(src, width, height, code, row, stride)
        finally:
            _unlock(self._lock_file)
        return True

    def _publish(self, src, width, height, code, row, stride):
        published = self._u64[_W_PUBLISHED // 8]
        reading = self._u64[_R_READING // 8]
        busy = set()
        if published:
            busy.add(published & 3)
        if reading:
            busy.add(reading - 1)
        idx = next(i for i in range(3) if i not in busy)
        off = self._buffers_off + idx * self._capacity
        length = row * height
        if stride == row:
            self._view[off:off + length] = src[:length]
        else:
            for y in range(height):
                self._view[off + y * row:off + (y + 1) * row] = src[y * stride:y * stride + row]
        seq = (published >> 2) + 1   # the surface's sequence: a successor continues it
        meta = struct.pack("<QIIIIQQ", seq, width, height, row, code,
                           int(time.time() * 1_000_000), length)
        crc = zlib.crc32(self._view[off:off + length], zlib.crc32(meta)) & 0xFFFFFFFF
        m = _M_BASE + idx * _M_STRIDE
        self._view[m:m + 40] = meta
        struct.pack_into("<I", self._map, m + 40, crc)
        self._u64[_W_PID // 8] = os.getpid()
        self._u64[_W_PUBLISHED // 8] = (seq << 2) | idx
        self._u64[_W_PRESENTED // 8] += 1

    @property
    def sequence(self):
        """The surface's newest frame sequence, shared by every generation of the node."""
        return self._u64[_W_PUBLISHED // 8] >> 2

    def host(self):
        """The host's last status, or None when no host ever attached (or the record is torn)."""
        b = bytes(self._view[_S_BASE:_S_BASE + _S_LEN + 4])
        if zlib.crc32(b[:_S_LEN]) & 0xFFFFFFFF != struct.unpack_from("<I", b, _S_LEN)[0]:
            return None
        fields = struct.unpack_from("<QQIIIIQQI", b, 0)
        return HostStatus(*fields) if fields[0] else None

    def poll_input(self):
        """Routed input and window events since the last call (only what the contract grants)."""
        if not _try_lock(self._lock_file):
            return []
        try:
            return self._drain()
        finally:
            _unlock(self._lock_file)

    def _drain(self):
        tail = self._u64[_W_TAIL // 8]
        events = []
        for _ in range(self._ring_slots):
            off = self._ring_off + (tail % self._ring_slots) * _SLOT
            b = bytes(self._view[off:off + _SLOT])
            if zlib.crc32(b[:56]) & 0xFFFFFFFF != struct.unpack_from("<I", b, 56)[0]:
                break
            seq, kind, mods, code, x, y, dx, dy = struct.unpack_from("<QHHIffff", b, 0)
            if seq <= tail:
                break
            if seq > tail + 1:
                self.lost_events += seq - tail - 1
            tail = seq
            text = b[32:56].split(b"\0", 1)[0].decode("utf-8", "replace")
            events.append(InputEvent(_KINDS.get(kind, "unknown"), mods, code, x, y, dx, dy, text))
        self._u64[_W_TAIL // 8] = tail
        return events

    def close(self):
        self._u64.release()
        self._view.release()
        self._map.close()
        self._file.close()
        self._lock_file.close()


_surfaces = None


def surfaces():
    """This node's surfaces (empty when it runs headless)."""
    global _surfaces
    if _surfaces is None:
        raw = os.environ.get("MVF_DISPLAY_SURFACES", "")
        _surfaces = [Surface(p) for p in raw.split(os.pathsep) if p]
    return _surfaces
