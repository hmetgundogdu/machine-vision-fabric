"""Benchmark module: touches only the first and last byte of the frame.

Isolates the data-plane cost — shared-memory handoff + stdio round-trip — with ~no per-frame
compute, so throughput reflects the architecture, not Python work.
"""
from mvf_sdk import run_classifier


def classify(payload, meta):
    memory = payload.memory
    n = len(memory)
    _ = (memory[0] + memory[n - 1]) if n else 0  # force a real read of the shared buffer
    return ("seen", float(n), "bytes", None)


run_classifier("py.bench-touch", classify)
