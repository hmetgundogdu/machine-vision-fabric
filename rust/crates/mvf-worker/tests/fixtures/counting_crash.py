"""Stateful counter that crashes once (exit 5) at count 7 — unless it was restored from a checkpoint,
so a restarted, restored instance carries on. Models a transient native fault in a stateful node."""
import os
import struct
from mvf_sdk import run_classifier, blob

_state = {"count": 0, "restored": False}


def classify(payload, meta):
    _state["count"] += 1
    if _state["count"] == 7 and not _state["restored"]:
        os._exit(5)
    return ("count", float(_state["count"]), "count", None)


def on_checkpoint():
    return blob(struct.pack("<q", _state["count"]))


def on_restore(payload):
    _state["count"] = struct.unpack("<q", bytes(payload.memory))[0]
    _state["restored"] = True


run_classifier("test.counting-crash", classify, on_checkpoint=on_checkpoint, on_restore=on_restore)
