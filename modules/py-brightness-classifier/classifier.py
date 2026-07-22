"""Sample polyglot module: a Python frame classifier.

Labels a frame "black" or "ok" by mean byte value (a dependency-free brightness proxy),
and reports the mean as a measurement. Demonstrates the perception->control bridge running
out-of-process in Python, plugged into the same typed graph as .NET nodes.

Run by the engine's worker host; PYTHONPATH points at src/sdk/python so `mvf_sdk` imports.
"""
from mvf_sdk import run_classifier

THRESHOLD = 10.0


def classify(data, frame):
    if not data:
        return ("unknown", None, None, "empty frame")
    mean = sum(data) / len(data)
    label = "black" if mean <= THRESHOLD else "ok"
    return (label, mean, "mean-byte", "n={}".format(len(data)))


run_classifier("py.brightness-classifier", classify)
