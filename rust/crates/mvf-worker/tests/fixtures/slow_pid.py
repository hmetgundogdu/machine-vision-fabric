"""Stateless, slow (20 ms per frame) classifier reporting its pid: replicable, so N instances in N
processes should share the work, and the engine must still deliver results in source order."""
import os
import time
from mvf_sdk import run_classifier


def classify(payload, meta):
    time.sleep(0.02)
    return ("pid", float(os.getpid()), "pid", str(meta.get("sequence")))


run_classifier("test.slow-pid", classify)
