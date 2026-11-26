"""Exits with code 7 on the second frame (native-style fault: no error reply)."""
import os
from mvf_sdk import run_classifier

seen = 0


def classify(payload, meta):
    global seen
    seen += 1
    if seen == 2:
        os._exit(7)
    return ("ok", None, None, None)


run_classifier("test.crash-on-execute", classify)
