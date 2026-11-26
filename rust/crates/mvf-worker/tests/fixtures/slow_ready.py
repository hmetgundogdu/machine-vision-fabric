"""Warms up for 300 ms, then signals ready (sd_notify READY=1 style)."""
import time
from mvf_sdk import run_classifier


def on_start():
    time.sleep(0.3)


run_classifier("test.slow-ready", lambda p, m: ("warm", None, None, None), on_start=on_start)
