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


def _lock(f):
    if os.name == "nt":
        import msvcrt
        f.seek(0)
        msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)


class Surface:
    """The node side of one surface (one window)."""

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
        self._owned = False
        self._last_try = None
        self._seq = 0
        self._tail = 0
        self.lost_events = 0

    def owned(self):
        """Whether this process owns the surface. During a live replacement the other generation
        does; presenting is then a no-op until it lets go (checked at most every 50 ms)."""
        if self._owned:
            return True
        now = time.monotonic()
        # monotonic() may start near zero, so "never tried" is None, not 0.0.
        if self._last_try is not None and now - self._last_try < 0.05:
            return False
        self._last_try = now
        try:
            _lock(self._lock_file)
        except OSError:
            return False
        self._owned = True
        self._seq = self._u64[_W_PUBLISHED // 8] >> 2
        self._tail = self._u64[_W_TAIL // 8]
        self._u64[_W_PID // 8] = os.getpid()
        return True

    def present(self, data, width, height, format="bgra8", stride=None):
        """Publish one frame. Returns True when published, False on standby. Never blocks."""
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
        if not self.owned():
            return False
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
        self._seq += 1
        meta = struct.pack("<QIIIIQQ", self._seq, width, height, row, code,
                           int(time.time() * 1_000_000), length)
        crc = zlib.crc32(self._view[off:off + length], zlib.crc32(meta)) & 0xFFFFFFFF
        m = _M_BASE + idx * _M_STRIDE
        self._view[m:m + 40] = meta
        struct.pack_into("<I", self._map, m + 40, crc)
        self._u64[_W_PUBLISHED // 8] = (self._seq << 2) | idx
        self._u64[_W_PRESENTED // 8] += 1
        return True

    def host(self):
        """The host's last status, or None when no host ever attached (or the record is torn)."""
        b = bytes(self._view[_S_BASE:_S_BASE + _S_LEN + 4])
        if zlib.crc32(b[:_S_LEN]) & 0xFFFFFFFF != struct.unpack_from("<I", b, _S_LEN)[0]:
            return None
        fields = struct.unpack_from("<QQIIIIQQI", b, 0)
        return HostStatus(*fields) if fields[0] else None

    def poll_input(self):
        """Routed input and window events since the last call (only what the contract grants)."""
        if not self.owned():
            return []
        events = []
        for _ in range(self._ring_slots):
            off = self._ring_off + (self._tail % self._ring_slots) * _SLOT
            b = bytes(self._view[off:off + _SLOT])
            if zlib.crc32(b[:56]) & 0xFFFFFFFF != struct.unpack_from("<I", b, 56)[0]:
                break
            seq, kind, mods, code, x, y, dx, dy = struct.unpack_from("<QHHIffff", b, 0)
            if seq <= self._tail:
                break
            if seq > self._tail + 1:
                self.lost_events += seq - self._tail - 1
            self._tail = seq
            text = b[32:56].split(b"\0", 1)[0].decode("utf-8", "replace")
            events.append(InputEvent(_KINDS.get(kind, "unknown"), mods, code, x, y, dx, dy, text))
        self._u64[_W_TAIL // 8] = self._tail
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
