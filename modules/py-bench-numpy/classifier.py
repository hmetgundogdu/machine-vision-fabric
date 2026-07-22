"""Benchmark module: reads the WHOLE frame via numpy (zero-copy view) and means it.

Shows realistic full-frame processing at C speed over the shared buffer — no copy, no base64.
"""
from mvf_sdk import run_classifier


def classify(payload, meta):
    array = payload.numpy()  # zero-copy uint8 view over the shared-memory buffer
    return ("seen", float(array.mean()), "mean", None)


run_classifier("py.bench-numpy", classify)
