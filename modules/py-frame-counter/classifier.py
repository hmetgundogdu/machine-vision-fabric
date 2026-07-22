"""Sample STATEFUL polyglot module: counts frames across cycles.

Labels each frame "even"/"odd" by the running count and reports the count as the measurement. Its
state (the count) survives a worker restart via on_checkpoint/on_restore — the engine captures it into
a shared-memory slot and restores it into a fresh worker (no base64). Demonstrates resume-after-crash.
PYTHONPATH points at src/sdk/python so `mvf_sdk` imports.
"""
import struct

from mvf_sdk import run_classifier, blob

_state = {"count": 0}


def classify(payload, meta):
    _state["count"] += 1
    count = _state["count"]
    label = "even" if count % 2 == 0 else "odd"
    return (label, float(count), "count", "seen {}".format(count))


def on_checkpoint():
    return blob(struct.pack("<q", _state["count"]))


def on_restore(payload):
    _state["count"] = struct.unpack("<q", bytes(payload.memory))[0]


run_classifier("py.frame-counter", classify, on_checkpoint=on_checkpoint, on_restore=on_restore)
