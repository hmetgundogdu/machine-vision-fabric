"""Sample polyglot module: a Python frame classifier.

Labels a frame "black" or "ok" by mean byte value (a dependency-free brightness proxy),
and reports the mean as a measurement. Demonstrates the perception->control bridge running
out-of-process in Python, plugged into the same typed graph as .NET nodes.

The frame arrives as a zero-copy typed Payload over shared memory (no base64); this module
reads its bytes directly. PYTHONPATH points at src/sdk/python so `mvf_sdk` imports.
"""
from mvf_sdk import run_classifier

THRESHOLD = 10.0


def classify(payload, meta):
    memory = payload.memory
    if len(memory) == 0:
        return ("unknown", None, None, "empty frame")
    mean = sum(memory) / len(memory)
    label = "black" if mean <= THRESHOLD else "ok"
    return (label, mean, "mean-byte", "n={}".format(len(memory)))


run_classifier("py.brightness-classifier", classify)
