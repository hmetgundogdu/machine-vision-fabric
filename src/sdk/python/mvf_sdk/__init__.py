"""MVF Python module SDK — control plane over stdio, data plane over shared memory.

A payload is **byte-based and typed**: it lives in the shared-memory arena as
``[descriptor header | payload bytes]`` and reaches the module as a zero-copy
:class:`Payload` (a ``memoryview`` you can also wrap as a numpy array). The type is
self-describing — media type, dtype and shape come from the header — so a module author
knows exactly what an edge carries. There is **no base64**; payloads never travel inline.

A classifier module is a function ``classify(payload: Payload, meta: dict) -> tuple``
returning ``(label, measurement, unit, details)``. The SDK owns the stdio loop and the
descriptor decoding. See ``../../../protocol/README.md``. Local only — no network.
"""
import sys
import os
import json
import mmap
import struct

__all__ = ["run_classifier", "run_processor", "log", "Payload", "Output", "blob", "tensor", "MediaType", "ElementType"]

# ---- typed-payload descriptor (must match Mvf.Abstractions.PayloadDescriptor) ----

_MAGIC = 0x3146564D          # 'MVF1' little-endian
_VERSION = 1
HEADER_SIZE = 192
_MAX_RANK = 8


class MediaType:
    BLOB = 0
    TENSOR = 1
    IMAGE = 2
    JSON = 3


class ElementType:
    UINT8 = 0
    INT8 = 1
    UINT16 = 2
    INT16 = 3
    UINT32 = 4
    INT32 = 5
    UINT64 = 6
    INT64 = 7
    FLOAT16 = 8
    BFLOAT16 = 9
    FLOAT32 = 10
    FLOAT64 = 11


# element type -> numpy dtype string (bfloat16 has no native numpy dtype and is omitted)
_NUMPY_DTYPE = {
    ElementType.UINT8: "u1", ElementType.INT8: "i1",
    ElementType.UINT16: "u2", ElementType.INT16: "i2",
    ElementType.UINT32: "u4", ElementType.INT32: "i4",
    ElementType.UINT64: "u8", ElementType.INT64: "i8",
    ElementType.FLOAT16: "f2", ElementType.FLOAT32: "f4", ElementType.FLOAT64: "f8",
}

_ELEMENT_SIZE = {
    ElementType.UINT8: 1, ElementType.INT8: 1,
    ElementType.UINT16: 2, ElementType.INT16: 2,
    ElementType.UINT32: 4, ElementType.INT32: 4,
    ElementType.UINT64: 8, ElementType.INT64: 8,
    ElementType.FLOAT16: 2, ElementType.BFLOAT16: 2, ElementType.FLOAT32: 4, ElementType.FLOAT64: 8,
}


class Payload:
    """A typed, byte-based payload read in place from the arena (zero copy)."""

    def __init__(self, media_type, element_type, shape, memory):
        self.media_type = media_type      # MediaType.*
        self.element_type = element_type  # ElementType.*
        self.shape = shape                # tuple of ints
        self.memory = memory              # memoryview over the arena slot (zero copy)

    def __len__(self):
        return len(self.memory)

    def numpy(self):
        """Wrap the payload as a numpy array with no copy (requires numpy)."""
        import numpy as np
        dtype = _NUMPY_DTYPE.get(self.element_type)
        if dtype is None:
            raise ValueError("element type {} has no numpy dtype".format(self.element_type))
        return np.frombuffer(self.memory, dtype=dtype).reshape(self.shape or (len(self.memory) // np.dtype(dtype).itemsize,))

    def bytes(self):
        return bytes(self.memory)


def _read_descriptor(buffer, offset):
    """Parse and structurally validate a descriptor header at ``offset``. Returns (media, elem, shape, length)."""
    magic, version = struct.unpack_from("<IH", buffer, offset)
    if magic != _MAGIC or version != _VERSION:
        raise ValueError("bad payload header (magic/version)")
    media_type = struct.unpack_from("<H", buffer, offset + 12)[0]
    element_type = buffer[offset + 14]
    rank = buffer[offset + 15]
    if rank > _MAX_RANK:
        raise ValueError("payload rank {} exceeds max".format(rank))
    payload_length = struct.unpack_from("<q", buffer, offset + 16)[0]
    shape = tuple(struct.unpack_from("<q", buffer, offset + 24 + i * 8)[0] for i in range(rank))
    return media_type, element_type, shape, payload_length


def _write_descriptor(buffer, offset, media_type, element_type, shape, payload, capacity):
    """Write a descriptor header + payload at ``offset`` (must match Mvf.Abstractions.PayloadDescriptor)."""
    rank = len(shape)
    if rank > _MAX_RANK:
        raise ValueError("output rank {} exceeds max".format(rank))
    element_size = _ELEMENT_SIZE[element_type]
    length = element_size
    for dim in shape:
        length *= dim
    if length != len(payload):
        raise ValueError("output payload is {} bytes but the descriptor implies {}".format(len(payload), length))
    if length > capacity:
        raise ValueError("output payload {} exceeds the reserved slot capacity {}".format(length, capacity))

    strides = [0] * rank
    running = element_size
    for i in range(rank - 1, -1, -1):
        strides[i] = running
        running *= shape[i]

    struct.pack_into("<I", buffer, offset, _MAGIC)
    struct.pack_into("<H", buffer, offset + 4, _VERSION)
    buffer[offset + 6] = 1  # flags: C-contiguous
    struct.pack_into("<I", buffer, offset + 8, 0)  # epoch
    struct.pack_into("<H", buffer, offset + 12, media_type)
    buffer[offset + 14] = element_type
    buffer[offset + 15] = rank
    struct.pack_into("<q", buffer, offset + 16, length)
    for i in range(rank):
        struct.pack_into("<q", buffer, offset + 24 + i * 8, shape[i])
        struct.pack_into("<q", buffer, offset + 88 + i * 8, strides[i])
    buffer[offset + HEADER_SIZE:offset + HEADER_SIZE + length] = payload


class Output:
    """A typed payload a transformer emits. Use :func:`blob` or :func:`tensor` to build one."""

    __slots__ = ("data", "media_type", "element_type", "shape")

    def __init__(self, data, media_type, element_type, shape):
        self.data = data  # bytes-like
        self.media_type = media_type
        self.element_type = element_type
        self.shape = tuple(shape)


def _as_bytes(data):
    if isinstance(data, (bytes, bytearray)):
        return bytes(data)
    return bytes(memoryview(data).cast("B"))


def blob(data):
    """A raw byte blob (u8)."""
    payload = _as_bytes(data)
    return Output(payload, MediaType.BLOB, ElementType.UINT8, (len(payload),))


def tensor(data, element_type, shape, media_type=MediaType.TENSOR):
    """A typed tensor: bytes-like ``data`` with an ``element_type`` and ``shape``."""
    return Output(_as_bytes(data), media_type, element_type, tuple(shape))


# ---- stdio control loop ----

def _send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def log(message, level="info"):
    """Emit a diagnostic log line to the engine (protocol ``{"type":"log"}``).

    Appears in the CLI dashboard (the node's log panel) and, headless, on the engine's stderr — the
    way a module reports what it is doing without polluting the typed data plane. Safe to call from
    ``transform``/``classify``/``on_start``: the stdio loop is single-threaded, so the line is written
    in order and never collides with a result. ``level`` is ``debug``/``info``/``warn``/``error``.
    """
    _send({"type": "log", "level": str(level), "message": str(message)})


def _open_arena(writable=False):
    path = os.environ.get("MVF_ARENA_PATH")
    if not path:
        return None
    flags = os.O_RDWR if writable else os.O_RDONLY
    access = mmap.ACCESS_WRITE if writable else mmap.ACCESS_READ
    fd = os.open(path, flags)
    try:
        return mmap.mmap(fd, 0, access=access)
    finally:
        os.close(fd)


def _read_input(arena_view, frame):
    shm = frame.get("shm")
    if shm is None:
        raise ValueError("frame is missing its shared-memory handle")
    offset = int(shm["offset"])
    media_type, element_type, shape, length = _read_descriptor(arena_view, offset)
    start = offset + HEADER_SIZE
    return media_type, element_type, shape, arena_view[start:start + length]


def _do_checkpoint(msg, request_id, arena_view, on_checkpoint):
    output = on_checkpoint() if on_checkpoint is not None else None
    if output is None:
        _send({"type": "state", "id": request_id, "empty": True})
        return
    out = msg["out"]
    _write_descriptor(
        arena_view, int(out["offset"]), output.media_type, output.element_type,
        output.shape, _as_bytes(output.data), int(out["capacity"]))
    _send({"type": "state", "id": request_id})


def _do_restore(msg, request_id, arena_view, on_restore):
    media_type, element_type, shape, cell = _read_input(arena_view, msg)
    try:
        if on_restore is not None:
            on_restore(Payload(media_type, element_type, shape, cell))
    finally:
        cell.release()
    _send({"type": "restored", "id": request_id})


def _serve(module_id, capability, writable, on_execute, on_checkpoint, on_restore, on_start=None):
    """Shared stdio loop: handshake, dispatch execute + checkpoint/restore, per-request error isolation.

    Readiness (sd_notify-style, see protocol/README.md): when ``on_start`` is given, the module warms up
    (e.g. loads a model, connects a device) *after* the ``hello`` handshake and signals ``ready`` when done.
    The ``hello`` carries ``"ready": false`` so the engine waits (bounded by its startup budget) — a slow
    warmup is a startup concern, not a liveness failure. With no ``on_start`` the module is ready at once.
    """
    arena = _open_arena(writable=writable)
    if arena is None:
        raise RuntimeError("MVF_ARENA_PATH is not set — the shared-memory data plane is required.")
    arena_view = memoryview(arena)
    try:
        if on_start is not None:
            _send({"type": "hello", "protocol": 1, "moduleId": module_id, "capability": capability, "ready": False})
            on_start()  # warmup: load model / connect device / init
            _send({"type": "ready", "moduleId": module_id})
        else:
            _send({"type": "hello", "protocol": 1, "moduleId": module_id, "capability": capability})
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            msg = json.loads(line)
            msg_type = msg.get("type")
            if msg_type == "shutdown":
                break

            request_id = msg.get("id")
            try:
                if msg_type == "execute":
                    on_execute(msg, request_id, arena_view)
                elif msg_type == "checkpoint":
                    _do_checkpoint(msg, request_id, arena_view, on_checkpoint)
                elif msg_type == "restore":
                    _do_restore(msg, request_id, arena_view, on_restore)
            except Exception as exc:  # report per-request failure, keep serving
                _send({"type": "error", "id": request_id, "message": str(exc)})
    finally:
        arena_view.release()
        arena.close()


def run_classifier(module_id, classify, on_checkpoint=None, on_restore=None, on_start=None):
    """Run the stdio loop for a classifier module.

    ``classify(payload, meta)`` returns ``(label, measurement, unit, details)`` (last three optional).
    Optional ``on_checkpoint() -> Output|None`` and ``on_restore(payload)`` make the module's state
    survive a restart (see :func:`blob` / :func:`tensor`). Optional ``on_start()`` runs warmup after the
    handshake (load a model / connect a device); the module signals ``ready`` when it returns.
    """
    def on_execute(msg, request_id, arena_view):
        frame = msg.get("frame") or {}
        media_type, element_type, shape, cell = _read_input(arena_view, frame)
        try:
            label, measurement, unit, details = classify(Payload(media_type, element_type, shape, cell), frame)
        finally:
            cell.release()
        _send({
            "type": "result",
            "id": request_id,
            "classification": {"label": label, "measurement": measurement, "unit": unit, "details": details},
        })

    writable = on_checkpoint is not None or on_restore is not None
    _serve(module_id, "classifier", writable, on_execute, on_checkpoint, on_restore, on_start)


def run_processor(module_id, transform, on_checkpoint=None, on_restore=None, on_start=None):
    """Run the stdio loop for a transformer module (frame in -> new frame out).

    ``transform(payload, meta)`` returns an :class:`Output` (see :func:`blob` / :func:`tensor`) or
    ``None`` to drop the frame. Optional ``on_checkpoint``/``on_restore`` persist module state. Optional
    ``on_start()`` runs warmup after the handshake; the module signals ``ready`` when it returns.
    """
    def on_execute(msg, request_id, arena_view):
        frame = msg.get("frame") or {}
        media_type, element_type, shape, cell = _read_input(arena_view, frame)
        try:
            output = transform(Payload(media_type, element_type, shape, cell), frame)
        finally:
            cell.release()

        if output is None:
            _send({"type": "result", "id": request_id, "frame": None})
            return
        out = msg["out"]
        _write_descriptor(
            arena_view, int(out["offset"]), output.media_type, output.element_type,
            output.shape, _as_bytes(output.data), int(out["capacity"]))
        _send({"type": "result", "id": request_id, "frame": {"shm": {"offset": int(out["offset"])}}})

    _serve(module_id, "processor", True, on_execute, on_checkpoint, on_restore, on_start)
