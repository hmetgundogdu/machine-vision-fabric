"""Hangs forever inside execute (a stuck native call)."""
import time
from mvf_sdk import run_classifier


def classify(payload, meta):
    while True:
        time.sleep(1)


run_classifier("test.hang", classify)
