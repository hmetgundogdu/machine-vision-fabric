"""Minimal MVF Python module SDK (control plane over stdio, newline-delimited JSON).

A classifier module is just a function ``classify(data: bytes, frame: dict) -> tuple``
returning ``(label, measurement, unit, details)``. The SDK owns the stdio message loop.

Frame bytes arrive one of two ways (see ../../../protocol/README.md):
- **shared memory (M2):** the engine sets ``MVF_ARENA_PATH`` to a file it memory-maps; a frame's
  ``shm`` handle ``{offset, length}`` points into it and we read the bytes in place (no copy off a
  pipe, no base64).
- **inline (M1 fallback):** ``dataBase64`` carries the bytes when there is no arena.

Local only — no network.
"""
import sys
import os
import json
import base64
import mmap

__all__ = ["run_classifier"]


def _send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def _open_arena():
    """Map the shared-memory arena read-only if the engine provided one; else None."""
    path = os.environ.get("MVF_ARENA_PATH")
    if not path:
        return None
    try:
        fd = os.open(path, os.O_RDONLY)
        try:
            # length 0 maps the whole file; both processes back onto the same pages.
            return mmap.mmap(fd, 0, access=mmap.ACCESS_READ)
        finally:
            os.close(fd)
    except OSError:
        return None


def _read_frame_data(frame, arena):
    """Extract the frame payload bytes from either a shm handle or an inline base64 field."""
    shm = frame.get("shm")
    if shm is not None and arena is not None:
        offset = int(shm["offset"])
        length = int(shm["length"])
        return bytes(arena[offset:offset + length])
    encoded = frame.get("dataBase64")
    if encoded:
        return base64.b64decode(encoded)
    return b""


def run_classifier(module_id, classify):
    """Run the stdio loop for a classifier module.

    ``classify(data, frame)`` must return ``(label, measurement, unit, details)``;
    ``measurement``/``unit``/``details`` may be ``None``.
    """
    arena = _open_arena()
    try:
        _send({"type": "hello", "protocol": 1, "moduleId": module_id, "capability": "classifier"})
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            msg = json.loads(line)
            msg_type = msg.get("type")
            if msg_type == "shutdown":
                break
            if msg_type != "execute":
                continue
            try:
                frame = msg.get("frame") or {}
                data = _read_frame_data(frame, arena)
                label, measurement, unit, details = classify(data, frame)
                _send({
                    "type": "result",
                    "id": msg.get("id"),
                    "classification": {
                        "label": label,
                        "measurement": measurement,
                        "unit": unit,
                        "details": details,
                    },
                })
            except Exception as exc:  # report per-request failure, keep serving
                _send({"type": "error", "id": msg.get("id"), "message": str(exc)})
    finally:
        if arena is not None:
            arena.close()
