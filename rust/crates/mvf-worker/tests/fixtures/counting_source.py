"""A source worker: produces five 16-byte frames (byte value = sequence), then reports exhaustion."""
from mvf_sdk import run_source, source_frame

_state = {"n": 0}


def produce():
    _state["n"] += 1
    n = _state["n"]
    if n > 5:
        return None
    return source_frame(bytes([n]) * 16, camera_id="py-cam", sequence=n)


run_source("test.counting-source", produce)
