"""Kills itself with SIGSEGV on the first frame (Unix): a real native fault, not an exception."""
import os
import signal
from mvf_sdk import run_classifier


def classify(payload, meta):
    os.kill(os.getpid(), signal.SIGSEGV)
    return ("unreachable", None, None, None)


run_classifier("test.segfault", classify)
