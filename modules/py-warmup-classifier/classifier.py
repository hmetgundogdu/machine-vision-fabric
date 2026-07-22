"""A classifier that warms up before it is ready (readiness demo, L.2).

`on_start` stands in for a real warmup — loading an ML model, connecting a camera, mapping a
device. Its duration (default ~50ms) can be overridden with MVF_WARMUP_MS so a test can drive a
warmup that overruns the engine's startup budget. The SDK sends `hello` with `"ready": false`,
runs `on_start`, then signals `ready`; the engine waits (bounded) before routing frames here.
"""
import os
import time

import mvf_sdk


def on_start():
    warmup_ms = float(os.environ.get("MVF_WARMUP_MS", "50"))
    time.sleep(warmup_ms / 1000.0)


def classify(payload, meta):
    # Trivial: report the frame size once we are warm.
    return ("ok", float(len(payload)), "bytes", "warmed up")


mvf_sdk.run_classifier("py.warmup-classifier", classify, on_start=on_start)
